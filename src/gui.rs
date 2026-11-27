//! The window (egui/eframe), in the fpvOS web flasher's look: the goggle HUD's
//! KESTREL theme (Chakra Petch, navy ground, lime accent, cyan data). One
//! column: the brand bar, a headline that says what to do now, and one panel
//! with the connected unit, the steps of the recovery as they happen, and the
//! one button that matters. The raw log sits under Details.
//!
//! The window runs as the normal user. On macOS, where claiming USB needs root,
//! it launches an elevated `--worker` (one administrator prompt) and tails its
//! status file; otherwise it runs the flow in-process over a channel.

use crate::engine::{self, Board, Detected, Step, Update, FLASHER_URL};
use crate::shot::Shot;
use eframe::egui::{
    self, text::LayoutJob, vec2, Align, Color32, CursorIcon, FontFamily, FontId, Layout, Margin, Rect, RichText, Sense,
    Stroke, TextFormat,
};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

// KESTREL palette (the flasher's CSS variables).
const GROUND: Color32 = Color32::from_rgb(0x06, 0x20, 0x2f);
const TEXT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
const DATA: Color32 = Color32::from_rgb(0x00, 0xe5, 0xff);
const ACCENT: Color32 = Color32::from_rgb(0xb3, 0xff, 0x00);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0xc6, 0xff, 0x40);
const QUIET: Color32 = Color32::from_rgb(0x8c, 0x9e, 0xb3);
const WARN: Color32 = Color32::from_rgb(0xff, 0xb0, 0x21);
const ERROR: Color32 = Color32::from_rgb(0xff, 0x5a, 0x52);
const PANEL: Color32 = Color32::from_rgb(0x0f, 0x2a, 0x3a);
const LINE: Color32 = Color32::from_rgb(0x24, 0x3d, 0x4f);
const FAINT: Color32 = Color32::from_rgb(0x3d, 0x55, 0x69);
const SUNK: Color32 = Color32::from_rgb(0x04, 0x17, 0x22);

/// `a` moved toward `b` by `t` (opaque colours, so boxes tint rather than grey).
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// A tinted note box: dark fill, coloured edge.
fn note_box(ui: &mut egui::Ui, tint: Color32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(mix(SUNK, tint, 0.08))
        .stroke(Stroke::new(1.0_f32, mix(SUNK, tint, 0.5)))
        .rounding(8.0)
        .inner_margin(Margin::symmetric(14.0, 11.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

const CHAKRA_MED: &[u8] = include_bytes!("../fonts/ChakraPetch-Medium.ttf");
const CHAKRA_SB: &[u8] = include_bytes!("../fonts/ChakraPetch-SemiBold.ttf");

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn sb() -> FontFamily {
    FontFamily::Name("sb".into())
}

/// Semibold text, as the flasher's headings.
fn strong(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text).family(sb()).size(size).color(color)
}

/// Medium body text.
fn body(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text).size(size).color(color)
}

/// The flasher's small spaced capitals (panel heads, state words, buttons).
fn caps(text: &str, size: f32, color: Color32) -> LayoutJob {
    LayoutJob::single_section(
        text.to_uppercase(),
        TextFormat { font_id: FontId::new(size, sb()), color, extra_letter_spacing: size * 0.15, ..Default::default() },
    )
}

fn setup(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("chakra".into(), egui::FontData::from_static(CHAKRA_MED));
    fonts.font_data.insert("chakra-sb".into(), egui::FontData::from_static(CHAKRA_SB));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "chakra".into());
    // Semibold first, then every proportional fallback (arrows and other
    // symbols Chakra Petch lacks come from egui's bundled fonts).
    let mut semibold = vec!["chakra-sb".to_string()];
    semibold.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts.families.insert(sb(), semibold);
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = GROUND;
    v.window_fill = GROUND;
    v.extreme_bg_color = SUNK;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = DATA;
    v.selection.bg_fill = DATA.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);
    // One look regardless of the system appearance: egui follows the OS theme
    // and keeps a separate light style, so pin Dark and give both slots the
    // fpvOS style - otherwise light mode shows egui's stock light ground.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.set_visuals_of(theme, v.clone());
    }
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.scroll.floating = true;
        style.interaction.selectable_labels = false;
    });
}

// ---- Widgets ----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Solid,
    Outline,
}

/// A flasher-style button. Disabled, it is drawn as a faint outline rather
/// than a dimmed fill. Returns whether it was clicked.
fn button(ui: &mut egui::Ui, label: &str, look: Look, enabled: bool, width: Option<f32>) -> bool {
    let galley = ui.fonts(|f| f.layout_job(caps(label, 13.5, Color32::PLACEHOLDER)));
    let size = vec2(width.unwrap_or(galley.size().x + 48.0), 46.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let hovered = enabled && resp.hovered();
    let painter = ui.painter();
    let text = match (look, enabled) {
        (Look::Solid, true) => {
            painter.rect_filled(rect, 8.0, if hovered { ACCENT_HOVER } else { ACCENT });
            GROUND
        }
        (Look::Outline, true) => {
            painter.rect_stroke(rect, 8.0, Stroke::new(1.0_f32, if hovered { QUIET } else { FAINT }));
            if hovered {
                TEXT
            } else {
                QUIET
            }
        }
        (_, false) => {
            painter.rect_stroke(rect, 8.0, Stroke::new(1.0_f32, LINE));
            FAINT
        }
    };
    painter.galley(rect.center() - galley.size() / 2.0, galley, text);
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    enabled && resp.clicked()
}

/// A small text action in spaced capitals.
fn link(ui: &mut egui::Ui, label: &str, color: Color32) -> bool {
    let galley = ui.fonts(|f| f.layout_job(caps(label, 12.0, Color32::PLACEHOLDER)));
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(0.0, 4.0), Sense::click());
    let c = if resp.hovered() { TEXT } else { color };
    ui.painter().galley(rect.left_top() + vec2(0.0, 2.0), galley, c);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// The HUD's state word: a small square (or spinner) and spaced capitals.
fn state_word(ui: &mut egui::Ui, label: &str, color: Color32, busy: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 9.0;
        let (r, _) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::hover());
        if busy {
            egui::Spinner::new()
                .size(12.0)
                .color(color)
                .paint_at(ui, Rect::from_center_size(r.center(), vec2(12.0, 12.0)));
        } else {
            ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(8.0, 8.0)), 2.0, color);
        }
        ui.label(caps(label, 12.5, color));
    });
}

fn hline(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, LINE);
}

fn progress_bar(ui: &mut egui::Ui, frac: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(r, 3.0, SUNK);
    let mut fill = r;
    fill.set_width((r.width() * frac.clamp(0.0, 1.0)).max(6.0));
    ui.painter().rect_filled(fill, 3.0, ACCENT);
}

#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Pending,
    Active,
    Done,
    Failed,
}

/// One line of the step list: its mark, label and, while it runs, `extra`.
fn step_row(ui: &mut egui::Ui, mark: Mark, label: &str, extra: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let (r, _) = ui.allocate_exact_size(vec2(20.0, 22.0), Sense::hover());
        let c = r.center();
        let p = ui.painter();
        match mark {
            Mark::Pending => {
                p.circle_stroke(c, 7.0, Stroke::new(1.5_f32, FAINT));
            }
            Mark::Active => {
                egui::Spinner::new().size(16.0).color(DATA).paint_at(ui, Rect::from_center_size(c, vec2(16.0, 16.0)));
            }
            Mark::Done => {
                p.circle_filled(c, 8.5, ACCENT);
                let s = Stroke::new(2.0_f32, GROUND);
                p.line_segment([c + vec2(-3.8, 0.2), c + vec2(-1.2, 2.8)], s);
                p.line_segment([c + vec2(-1.2, 2.8), c + vec2(3.9, -2.6)], s);
            }
            Mark::Failed => {
                p.circle_filled(c, 8.5, ERROR);
                let s = Stroke::new(2.0_f32, GROUND);
                p.line_segment([c + vec2(-3.0, -3.0), c + vec2(3.0, 3.0)], s);
                p.line_segment([c + vec2(-3.0, 3.0), c + vec2(3.0, -3.0)], s);
            }
        }
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 5.0;
            let color = match mark {
                Mark::Pending => QUIET,
                Mark::Active => TEXT,
                Mark::Done => TEXT.gamma_multiply(0.82),
                Mark::Failed => ERROR,
            };
            ui.add_space(1.0);
            ui.label(body(label, 15.0, color));
            if mark == Mark::Active {
                extra(ui);
            }
        });
    });
}

fn step_label(step: Step) -> &'static str {
    match step {
        Step::Authorize => "Allow access to USB",
        Step::Reboot => "Restart the unit into its bootloader",
        Step::Driver => "Install the USB driver",
        Step::Verify => "Check the bootloader",
        Step::Load => "Load fpvOS recovery into memory",
        Step::Boot => "Start recovery mode",
    }
}

/// What the user should know (or do) while a step runs.
fn step_detail(step: Step) -> Option<&'static str> {
    match step {
        Step::Authorize => Some("Enter your Mac password in the dialog that opened."),
        Step::Reboot => Some("The unit disconnects and comes back as its bootloader."),
        Step::Driver => Some("Approve the Windows prompt. It happens once per computer."),
        Step::Verify | Step::Load | Step::Boot => None,
    }
}

fn fmt_eta(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 60 {
        format!("about {} min {:02} s left", s / 60, s % 60)
    } else {
        format!("about {s} s left")
    }
}

// ---- State ------------------------------------------------------------------

enum Phase {
    Idle,
    Running,
    Done { dry_run: bool },
    Failed(String),
}

enum RunSource {
    None,
    Channel(Receiver<Update>),
    File { file: std::fs::File, buf: String },
}

pub struct RecoveryApp {
    source: RunSource,
    det_rx: Option<Receiver<Detected>>,
    started_detect: bool,
    detected: Detected,
    board_pick: Option<Board>,
    phase: Phase,
    /// The unit being recovered: kept while running and after, whatever the
    /// live detection says as it reboots.
    run_board: Option<Board>,
    steps: Vec<(Step, Mark)>,
    progress: Option<f32>,
    load_start: Option<(Instant, usize)>,
    eta: Option<f32>,
    winusb: Option<String>,
    notice: Option<String>,
    log: Vec<String>,
    show_log: bool,
    /// When the run began (each log line says how long after), and where its
    /// log was saved once it ended.
    run_start: Option<Instant>,
    log_file: Option<std::path::PathBuf>,
    dry_run: bool,
    demo: bool,
    shot: Option<Shot>,
}

/// The Downloads folder of the user running the window.
fn home_downloads() -> Option<std::path::PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    let dir = std::path::PathBuf::from(home).join("Downloads");
    dir.is_dir().then_some(dir)
}

/// The time now in UTC, "YYYY-MM-DD HH:MM:SS", with no date library.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// AppleScript string literal.
fn asq(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl RecoveryApp {
    fn new(dry_run: bool, shot: Option<Shot>) -> Self {
        Self {
            source: RunSource::None,
            det_rx: None,
            started_detect: false,
            detected: Detected::None,
            board_pick: None,
            phase: Phase::Idle,
            run_board: None,
            steps: Vec::new(),
            progress: None,
            load_start: None,
            eta: None,
            winusb: None,
            notice: None,
            log: Vec::new(),
            show_log: false,
            run_start: None,
            log_file: None,
            dry_run,
            demo: false,
            shot,
        }
    }

    fn start_detection(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.det_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || loop {
            if tx.send(engine::detect()).is_err() {
                return;
            }
            ctx.request_repaint();
            std::thread::sleep(Duration::from_millis(1000));
        });
    }

    /// Back to the start screen, ready for this or another unit.
    fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.run_board = None;
        self.steps.clear();
        self.progress = None;
        self.load_start = None;
        self.eta = None;
        self.winusb = None;
        self.board_pick = None;
    }

    fn start_run(&mut self, ctx: &egui::Context, board: Board) {
        self.reset();
        self.log.clear();
        self.log_file = None;
        self.run_start = Some(Instant::now());
        self.note(format!(
            "fpvOS Recovery {} on {} {}, {} (UTC {}); the unit is a {}, handed off to {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            if self.dry_run { "dry run" } else { "recovery" },
            utc_now(),
            board.label(),
            FLASHER_URL
        ));
        self.notice = None;
        self.phase = Phase::Running;
        self.run_board = Some(board);
        let dry = self.dry_run;
        let elevate = engine::needs_elevation();
        if elevate {
            self.steps.push((Step::Authorize, Mark::Active));
        }
        for step in [Step::Reboot, Step::Verify, Step::Load, Step::Boot] {
            if !(dry && step == Step::Boot) {
                self.steps.push((step, Mark::Pending));
            }
        }

        if elevate {
            // macOS: run an elevated worker and tail its status file.
            let path = std::env::temp_dir().join(format!("fpvos-recovery-{}.log", std::process::id()));
            let _ = std::fs::File::create(&path); // truncate / create as the user
            match std::fs::File::open(&path) {
                Ok(file) => self.source = RunSource::File { file, buf: String::new() },
                Err(e) => {
                    self.fail(format!("Could not create the status file: {e}"));
                    return;
                }
            }
            let worker_path = path.to_string_lossy().into_owned();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let mut inner = format!("{} --worker --status {} --board {}", shq(&exe), shq(&worker_path), board.id());
                if dry {
                    inner.push_str(" --dry-run");
                }
                inner.push_str(" >/dev/null 2>&1");
                let script = format!(
                    "do shell script {} with prompt {} with administrator privileges",
                    asq(&inner),
                    asq("fpvOS Recovery needs your password to reach your unit over USB. \
                         Only the unit's memory is used; nothing is installed on this Mac.")
                );
                let out = std::process::Command::new("osascript").arg("-e").arg(&script).output();
                let (ok, err) = match &out {
                    Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned()),
                    Err(e) => (false, e.to_string()),
                };
                if !ok {
                    // If the worker never reached a terminal line, the prompt was
                    // dismissed (-128) or could not run: record it so the window
                    // resolves.
                    let had_terminal = std::fs::read_to_string(&worker_path)
                        .map(|s| s.lines().any(|l| l.starts_with("D\t") || l.starts_with("F\t")))
                        .unwrap_or(false);
                    if !had_terminal {
                        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&worker_path) {
                            let _ = if err.contains("-128") {
                                writeln!(f, "C")
                            } else {
                                writeln!(f, "F\tCould not get administrator access: {}", err.trim())
                            };
                        }
                    }
                }
                ctx.request_repaint();
            });
        } else {
            // Already root, or Windows: run in-process over a channel.
            let (tx, rx) = mpsc::channel();
            self.source = RunSource::Channel(rx);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let report = move |u: Update| {
                    let _ = tx.send(u);
                    ctx.request_repaint();
                };
                engine::run_recovery(dry, board, &report);
            });
        }
    }

    /// A step begins: the ones before it are done. A step not in the list yet
    /// (the Windows driver) goes in right after the one that was running.
    fn enter_step(&mut self, step: Step) {
        if !self.steps.iter().any(|(s, _)| *s == step) {
            let at = self.steps.iter().position(|(_, m)| *m == Mark::Active).map(|i| i + 1).unwrap_or(self.steps.len());
            self.steps.insert(at, (step, Mark::Pending));
        }
        let idx = self.steps.iter().position(|(s, _)| *s == step).unwrap_or(0);
        for (i, (_, mark)) in self.steps.iter_mut().enumerate() {
            *mark = if i < idx {
                Mark::Done
            } else if i == idx {
                Mark::Active
            } else {
                Mark::Pending
            };
        }
    }

    fn fail(&mut self, message: String) {
        for (_, mark) in self.steps.iter_mut() {
            if *mark == Mark::Active {
                *mark = Mark::Failed;
            }
        }
        self.note(format!("Error: {message}"));
        self.phase = Phase::Failed(message);
        // what to send when asking for help, in view
        self.show_log = true;
        self.progress = None;
    }

    /// A log line, with how long into the run it came.
    fn note(&mut self, line: String) {
        match self.run_start {
            Some(t0) => self.log.push(format!("{:>6.1}s  {line}", t0.elapsed().as_secs_f32())),
            None => self.log.push(line),
        }
    }

    /// The run's log, saved where the user finds it (Downloads, else the
    /// temporary folder), to send with a question or a bug report.
    fn save_log(&mut self) {
        let dir = home_downloads().unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("fpvOS-Recovery-{}.log", utc_now().replace([':', '-'], "").replace(' ', "-")));
        match std::fs::write(&path, self.log.join("\n") + "\n") {
            Ok(()) => {
                self.note(format!("Log saved to {}", path.display()));
                self.log_file = Some(path);
            }
            Err(e) => self.note(format!("Could not save the log to {}: {e}", path.display())),
        }
    }

    fn apply(&mut self, u: Update, finished: &mut bool) {
        match u {
            Update::Status(s) => self.note(s),
            Update::Step(step) => self.enter_step(step),
            Update::Progress { done, total, .. } => {
                self.progress = Some(done as f32 / total.max(1) as f32);
                let (t0, d0) = *self.load_start.get_or_insert((Instant::now(), done));
                let elapsed = t0.elapsed().as_secs_f32();
                if done > d0 && elapsed > 1.5 {
                    let rate = (done - d0) as f32 / elapsed;
                    self.eta = Some(total.saturating_sub(done) as f32 / rate);
                }
            }
            Update::NeedsWinusb { driver } => {
                self.note(format!("The bootloader is bound to {driver}, not WinUSB."));
                self.winusb = Some(driver);
            }
            Update::Finished { dry_run } => {
                for (_, mark) in self.steps.iter_mut() {
                    *mark = Mark::Done;
                }
                self.phase = Phase::Done { dry_run };
                self.progress = None;
                self.note("Finished.".into());
                self.save_log();
                *finished = true;
            }
            Update::Failed(e) => {
                self.fail(e);
                self.save_log();
                *finished = true;
            }
            Update::Cancelled => {
                self.reset();
                self.notice = Some(CANCELLED.into());
                *finished = true;
            }
        }
    }

    fn drain(&mut self) {
        if let Some(rx) = &self.det_rx {
            while let Ok(d) = rx.try_recv() {
                // A unit choice belongs to the unit it was made for: another
                // one plugged in (or the same one in another state) is asked anew.
                if d != self.detected {
                    self.board_pick = None;
                }
                self.detected = d;
            }
        }
        let mut updates: Vec<Update> = Vec::new();
        match &mut self.source {
            RunSource::Channel(rx) => {
                while let Ok(u) = rx.try_recv() {
                    updates.push(u);
                }
            }
            RunSource::File { file, buf } => {
                let mut s = String::new();
                let _ = file.read_to_string(&mut s);
                buf.push_str(&s);
                while let Some(nl) = buf.find('\n') {
                    let line: String = buf.drain(..=nl).collect();
                    if let Some(u) = engine::parse_line(line.trim_end()) {
                        updates.push(u);
                    }
                }
            }
            RunSource::None => {}
        }
        let mut finished = false;
        for u in updates {
            self.apply(u, &mut finished);
        }
        if finished {
            self.source = RunSource::None;
        }
    }

    /// The board a press of Start would recover.
    fn ready_board(&self) -> Option<Board> {
        match self.detected {
            Detected::Stock(b) => Some(b),
            // a pick made for one state never carries to the other
            Detected::StockOneOf(list) => self.board_pick.filter(|b| list.contains(b)),
            Detected::Bootloader => self.board_pick,
            Detected::None => None,
        }
    }
}

const CANCELLED: &str = "Recovery needs your Mac password to reach the unit over USB. Start again when you're ready.";

impl eframe::App for RecoveryApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(s) = &mut self.shot {
            s.tick(ctx);
        }
        if !self.started_detect && !self.demo {
            self.start_detection(ctx);
            self.started_detect = true;
        }
        self.drain();
        if matches!(self.phase, Phase::Running) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // macOS draws the content under a transparent title bar; leave room
        // for the window buttons.
        let top = if cfg!(target_os = "macos") { 40.0 } else { 22.0 };
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(GROUND).inner_margin(Margin { left: 0.0, right: 0.0, top, bottom: 0.0 }))
            .show(ctx, |ui| {
                if cfg!(target_os = "macos") {
                    // The hidden title bar's strip still moves the window.
                    let strip = Rect::from_min_size(ctx.screen_rect().min, vec2(ctx.screen_rect().width(), top));
                    let drag = ui.interact(strip, egui::Id::new("title-strip"), Sense::click_and_drag());
                    if drag.drag_started_by(egui::PointerButton::Primary) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let avail = ui.available_width();
                    let w = (avail - 64.0).clamp(280.0, 540.0);
                    let pad = ((avail - w) / 2.0).max(0.0);
                    ui.horizontal_top(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                            ui.set_width(w);
                            self.content(ui, ctx);
                            ui.add_space(28.0);
                        });
                    });
                });
            });
    }
}

// ---- Layout -------------------------------------------------------------------

impl RecoveryApp {
    fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.header(ui);
        ui.add_space(26.0);

        let (title, lead) = self.headline();
        ui.label(strong(title, 24.0, TEXT));
        ui.add_space(2.0);
        ui.label(body(lead, 15.0, QUIET));
        ui.add_space(20.0);

        egui::Frame::none()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, LINE))
            .rounding(12.0)
            .inner_margin(Margin::symmetric(22.0, 20.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.panel(ui, ctx);
            });

        if let Some(notice) = &self.notice {
            if matches!(self.phase, Phase::Idle) {
                ui.add_space(12.0);
                note_box(ui, WARN, |ui| {
                    ui.label(body(notice, 14.0, TEXT));
                });
            }
        }

        ui.add_space(18.0);
        self.footer(ui);
    }

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut brand = LayoutJob::default();
            let f = |color| TextFormat {
                font_id: FontId::new(23.0, sb()),
                color,
                extra_letter_spacing: 0.4,
                ..Default::default()
            };
            brand.append("fpv", 0.0, f(TEXT));
            brand.append("OS", 0.0, f(ACCENT));
            ui.label(brand);
            ui.add_space(4.0);
            ui.label(caps("Recovery", 12.5, QUIET));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(body(format!("v{VERSION}"), 12.5, FAINT));
                if self.dry_run {
                    ui.label(caps("Dry run", 11.0, WARN));
                }
            });
        });
        ui.add_space(12.0);
        hline(ui);
    }

    fn headline(&self) -> (&'static str, &'static str) {
        match &self.phase {
            Phase::Idle => match self.detected {
                Detected::None => (
                    "Connect your unit",
                    "Plug a Skyzone SteadyDigital or a BETAFPV P1 (Air, goggles or Nano VRX) running its stock firmware into this computer over USB.",
                ),
                Detected::StockOneOf(_) => (
                    "Which P1 is this?",
                    "This P1 on its stock firmware didn't say whether it is the air unit or a ground unit. Choose it below: each has its own recovery, and nothing checks the choice for you.",
                ),
                Detected::Stock(_) => (
                    "Ready to start recovery",
                    "The app restarts the unit and loads fpvOS recovery into its memory. You then install fpvOS from the fpvOS flasher.",
                ),
                Detected::Bootloader => (
                    "Which unit is this?",
                    "The unit is already in its bootloader, which doesn't say what it is. Choose it below.",
                ),
            },
            Phase::Running => ("Starting recovery mode", "Keep the unit plugged in until this finishes."),
            Phase::Done { dry_run: false } => (
                "Your unit is in recovery mode",
                "Go back to the fpvOS flasher in your browser and connect to the unit to install fpvOS.",
            ),
            Phase::Done { dry_run: true } => (
                "Dry run complete",
                "The bootloader was checked and recovery was loaded and verified in memory, but not started. Unplug the unit to return it to stock.",
            ),
            Phase::Failed(_) => (
                "Recovery didn't start",
                "Nothing was written to the unit. Unplug it, plug it back in, and try again.",
            ),
        }
    }

    fn panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // The unit.
        let shown = self.run_board.or(match self.detected {
            Detected::Stock(b) => Some(b),
            _ => None,
        });
        match (&self.phase, self.detected) {
            (Phase::Idle, Detected::None) => state_word(ui, "Searching", DATA, true),
            (Phase::Idle, Detected::Stock(_) | Detected::StockOneOf(_)) => state_word(ui, "Stock firmware", ACCENT, false),
            (Phase::Idle, Detected::Bootloader) => state_word(ui, "Bootloader", DATA, false),
            (Phase::Running, _) => state_word(ui, "Working", DATA, true),
            (Phase::Done { dry_run: false }, _) => state_word(ui, "Recovery mode", ACCENT, false),
            (Phase::Done { dry_run: true }, _) => state_word(ui, "Dry run done", ACCENT, false),
            (Phase::Failed(_), _) => state_word(ui, "Stopped", ERROR, false),
        }
        ui.add_space(2.0);
        // A stock P1 is named once the user has said which it is.
        let shown = shown.or(match (&self.phase, self.detected) {
            (Phase::Idle, Detected::StockOneOf(_)) => self.ready_board(),
            _ => None,
        });
        let name = match (shown, &self.phase, self.detected) {
            (Some(b), _, _) => b.label(),
            (None, Phase::Idle, Detected::StockOneOf(_)) => "BETAFPV P1",
            (None, Phase::Idle, Detected::Bootloader) => "Unknown unit",
            _ => "No unit connected",
        };
        ui.label(strong(name, 21.0, if shown.is_some() { TEXT } else { QUIET }));

        let choices: &[Board] = match self.detected {
            Detected::Bootloader => &Board::ALL,
            Detected::StockOneOf(list) => list,
            _ => &[],
        };
        if matches!(self.phase, Phase::Idle) && !choices.is_empty() {
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for &b in choices {
                    let look = if self.ready_board() == Some(b) { Look::Solid } else { Look::Outline };
                    if button(ui, b.label(), look, true, None) {
                        self.board_pick = Some(b);
                    }
                }
            });
        }

        // The steps while a run is going or has stopped; once it succeeds the
        // state word says it all.
        if !self.steps.is_empty() && matches!(self.phase, Phase::Running | Phase::Failed(_)) {
            ui.add_space(16.0);
            hline(ui);
            ui.add_space(14.0);
            let progress = self.progress;
            let eta = self.eta;
            let winusb = self.winusb.clone();
            for (step, mark) in self.steps.clone() {
                step_row(ui, mark, step_label(step), |ui| {
                    if let Some(detail) = step_detail(step) {
                        ui.label(body(detail, 13.5, QUIET));
                    }
                    if step == Step::Load {
                        if let Some(frac) = progress {
                            ui.add_space(2.0);
                            progress_bar(ui, frac);
                            let left = eta.map(fmt_eta).unwrap_or_else(|| "estimating time left".into());
                            ui.label(body(format!("{:.0}%  ·  {left}", frac * 100.0), 13.0, QUIET));
                        }
                    }
                    if step == Step::Driver {
                        if let Some(driver) = &winusb {
                            zadig(ui, driver);
                        }
                    }
                });
                ui.add_space(6.0);
            }
        }

        // The error, in full.
        if let Phase::Failed(message) = &self.phase {
            ui.add_space(8.0);
            note_box(ui, ERROR, |ui| {
                ui.label(body(message, 14.0, TEXT));
            });
        }

        // The one action that matters now.
        if matches!(self.phase, Phase::Running) {
            return;
        }
        ui.add_space(20.0);
        let full = Some(ui.available_width());
        match &self.phase {
            Phase::Idle => {
                let board = self.ready_board();
                if button(ui, "Start recovery", Look::Solid, board.is_some(), full) {
                    if let Some(b) = board {
                        self.start_run(ctx, b);
                    }
                }
                let hint = match self.detected {
                    Detected::Bootloader | Detected::StockOneOf(_) if board.is_none() => Some("Choose the unit above to continue."),
                    Detected::None => None,
                    _ if cfg!(target_os = "macos") => {
                        Some("macOS asks for your password next: it lets the app reach the unit over USB.")
                    }
                    _ if cfg!(target_os = "windows") => {
                        Some("The first time, Windows asks to install a USB driver for the unit.")
                    }
                    _ => None,
                };
                if let Some(hint) = hint {
                    ui.add_space(4.0);
                    ui.label(body(hint, 13.5, QUIET));
                }
            }
            Phase::Running => {}
            Phase::Done { dry_run: false } => {
                if button(ui, "Open the fpvOS flasher", Look::Solid, true, full) {
                    engine::open_url(FLASHER_URL);
                }
                ui.add_space(8.0);
                if link(ui, "Recover another unit", QUIET) {
                    self.reset();
                }
            }
            Phase::Done { dry_run: true } => {
                if button(ui, "Done", Look::Outline, true, full) {
                    self.reset();
                }
            }
            Phase::Failed(_) => {
                if button(ui, "Try again", Look::Solid, true, full) {
                    self.reset();
                }
                ui.add_space(8.0);
                if link(ui, "Copy the log for a bug report", DATA) {
                    ui.ctx().copy_text(self.log.join("\n"));
                }
                if let Some(path) = &self.log_file {
                    ui.add_space(4.0);
                    ui.label(body(&format!("Also saved as {}", path.display()), 12.5, QUIET));
                }
            }
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.label(body("Only the unit's memory is used: unplug it at any time to return it to stock.", 13.0, QUIET));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let label = if self.show_log { "Hide details" } else { "Show details" };
            if link(ui, label, QUIET) {
                self.show_log = !self.show_log;
            }
            if self.show_log && !self.log.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if link(ui, "Copy", DATA) {
                        ui.ctx().copy_text(self.log.join("\n"));
                    }
                });
            }
        });
        if self.show_log {
            ui.add_space(6.0);
            egui::Frame::none()
                .fill(SUNK)
                .stroke(Stroke::new(1.0_f32, LINE))
                .rounding(8.0)
                .inner_margin(Margin::symmetric(12.0, 10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(170.0)
                        .auto_shrink([false, true])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if self.log.is_empty() {
                                ui.label(RichText::new("Nothing yet.").monospace().size(12.0).color(FAINT));
                            }
                            for line in &self.log {
                                ui.label(RichText::new(line).monospace().size(12.0).color(QUIET));
                            }
                        });
                });
        }
    }
}

/// Windows fallback when the driver could not be installed automatically.
fn zadig(ui: &mut egui::Ui, driver: &str) {
    ui.add_space(4.0);
    note_box(ui, WARN, |ui| {
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.label(caps("Install it with Zadig", 12.0, WARN));
        let seen = if driver.is_empty() || driver == "no driver" {
            "no driver".to_string()
        } else {
            format!("the {driver} driver")
        };
        ui.label(body(
            format!("The automatic install didn't finish, and the bootloader still uses {seen}. Bind it once by hand:"),
            13.5,
            TEXT,
        ));
        ui.label(body("1.  Open Zadig.", 13.5, QUIET));
        ui.label(body("2.  Options › List All Devices, then pick “USB download gadget” (1D6B 0100).", 13.5, QUIET));
        ui.label(body("3.  Choose WinUSB and click Install Driver. This window carries on by itself.", 13.5, QUIET));
        if link(ui, "Get Zadig  ›", DATA) {
            engine::open_url("https://zadig.akeo.ie");
        }
    });
}

// ---- Design previews ----------------------------------------------------------

impl RecoveryApp {
    /// `--demo STATE`: a screen with made-up data, for checking the design
    /// without hardware. No detection or USB runs.
    fn demo(&mut self, which: &str) -> Result<(), String> {
        use Mark::*;
        use Step::*;
        self.demo = true;
        let log: Vec<String> = [
            "BETAFPV P1 Air recovery ready: kernel 5325568 bytes.",
            "Rebooting into the bootloader via /dev/cu.usbmodem1101…",
            "Waiting for the Artosyn USB download gadget…",
            "Bootloader detected — USB high speed.",
            "Verifying the bootloader…",
            "Bootloader verified (4 fingerprints + dispatch table).",
            "Loading device tree into RAM (0.0 MiB)…",
            "device tree: verified on-device (CRC32 1c0ffee5).",
            "Loading recovery initrd into RAM (2.9 MiB)…",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        match which {
            "idle" => {}
            "found" => self.detected = Detected::Stock(Board::P1Air),
            "bootloader" => self.detected = Detected::Bootloader,
            "stock-p1" => self.detected = Detected::StockOneOf(&Board::STOCK_P1),
            "stock-p1-gnd" => self.detected = Detected::Stock(Board::P1Gnd),
            "cancelled" => {
                self.detected = Detected::Stock(Board::SteadyDigital);
                self.notice = Some(CANCELLED.into());
            }
            "auth" => {
                self.phase = Phase::Running;
                self.run_board = Some(Board::P1Air);
                self.steps =
                    vec![(Authorize, Active), (Reboot, Pending), (Verify, Pending), (Load, Pending), (Boot, Pending)];
            }
            "loading" => {
                self.phase = Phase::Running;
                self.run_board = Some(Board::P1Air);
                self.steps = vec![(Authorize, Done), (Reboot, Done), (Verify, Done), (Load, Active), (Boot, Pending)];
                self.progress = Some(0.54);
                self.eta = Some(41.0);
                self.log = log;
                self.show_log = true;
            }
            "driver" => {
                self.phase = Phase::Running;
                self.run_board = Some(Board::SteadyDigital);
                self.steps =
                    vec![(Reboot, Done), (Driver, Active), (Verify, Pending), (Load, Pending), (Boot, Pending)];
                self.winusb = Some("HidUsb".into());
            }
            "done" => {
                self.phase = Phase::Done { dry_run: false };
                self.run_board = Some(Board::P1Air);
                self.steps = vec![(Authorize, Done), (Reboot, Done), (Verify, Done), (Load, Done), (Boot, Done)];
            }
            "dry" => {
                self.dry_run = true;
                self.phase = Phase::Done { dry_run: true };
                self.run_board = Some(Board::SteadyDigital);
                self.steps = vec![(Authorize, Done), (Reboot, Done), (Verify, Done), (Load, Done)];
            }
            "failed" => {
                self.phase =
                    Phase::Failed("The bootloader did not appear on USB within 30 seconds after the restart.".into());
                self.run_board = Some(Board::P1Air);
                self.steps =
                    vec![(Authorize, Done), (Reboot, Failed), (Verify, Pending), (Load, Pending), (Boot, Pending)];
                self.log = log[..3].to_vec();
                self.show_log = true;
                self.log_file = Some(std::path::PathBuf::from("C:\\Users\\pilot\\Downloads\\fpvOS-Recovery-20261127-050122.log"));
            }
            other => return Err(format!("unknown demo state '{other}'")),
        }
        Ok(())
    }
}

pub struct Options {
    pub dry_run: bool,
    pub demo: Option<String>,
    pub screenshot: Option<String>,
}

pub fn run(opts: Options) -> Result<(), String> {
    let mut app = RecoveryApp::new(opts.dry_run, opts.screenshot.map(Shot::new));
    if let Some(state) = &opts.demo {
        app.demo(state)?;
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([600.0, 720.0])
        .with_min_inner_size([440.0, 520.0])
        .with_title("fpvOS Recovery")
        // Dock / taskbar icon while running (the bundle and exe carry their own).
        .with_icon(
            eframe::icon_data::from_png_bytes(include_bytes!("../packaging/icon.png")).expect("packaging/icon.png"),
        );
    if cfg!(target_os = "macos") {
        // Content runs under a transparent title bar, as native Mac apps do.
        viewport = viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "fpvOS Recovery",
        options,
        Box::new(move |cc| {
            setup(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_now_is_a_plain_date_and_time() {
        let now = super::utc_now();
        assert_eq!(now.len(), 19, "{now}");
        assert!(now.starts_with("20"), "{now}");
        let year: i32 = now[..4].parse().unwrap();
        let month: u32 = now[5..7].parse().unwrap();
        let day: u32 = now[8..10].parse().unwrap();
        assert!(year >= 2026 && (1..=12).contains(&month) && (1..=31).contains(&day), "{now}");
        assert_eq!(&now[10..11], " ");
    }
}
