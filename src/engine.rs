//! The recovery flow, independent of any UI. It reports progress through an
//! `Update` stream so either the CLI or the GUI can drive it. All USB work
//! happens on whatever thread calls `run_recovery`.
//!
//! The SteadyDigital and a P1 on stock 2.x share the Artosyn "v2" U-Boot HID
//! loader; a P1 on stock 1.0.44 has the "v1" one, which the loader finds by
//! probing (v1 first). Otherwise only the stock USB id, the embedded recovery
//! bundle, and the loader's per-frame sizes differ.
//!
//! A P1 is the air unit or a ground unit (the VR04 goggles, the P1 HD VRX), each
//! kind with its own recovery. All have one stock USB id and one bootloader:
//! on stock the kind is read from the unit's USB interfaces (stock_p1), else
//! the user says. The ground units share one recovery, device tree included:
//! which of them it is comes from the fpvOS bundle installed afterwards, one
//! per unit, and the recovery takes either. The ground units are not yet run
//! on hardware.

use crate::loader::{Layout, Loader, DTB_ADDR, INITRD_ADDR, KERNEL_ADDR, V1, V2};

// Embedded recovery payloads.
const SD_KERNEL: &[u8] = include_bytes!("../assets/steadydigital-updater/browser-v1/kernel.img");
const SD_DTB: &[u8] = include_bytes!("../assets/steadydigital-updater/browser-v1/skyzone-steadydigital.dtb");
const SD_INITRD: &[u8] = include_bytes!("../assets/steadydigital-updater/browser-v1/recovery-initrd.img");
const SD_KERNEL_SHA: &str = "d27aa43cf3f981dc8b064db1a2226172615decb8d1d126b549ffacb43005a445";
const SD_DTB_SHA: &str = "5f14a74844890ad7dbe2f0769cc8ed1d41c0f5f751623e07f76729dd945ef05b";
const SD_INITRD_SHA: &str = "70d994a5c110d6be877007f4a371f8c01a6c5bd0a252767c3cfbc37c3d2f1858";

const P1_KERNEL: &[u8] = include_bytes!("../assets/p1-updater/browser-v1/kernel.img");
const P1_DTB: &[u8] = include_bytes!("../assets/p1-updater/browser-v1/betafpv-p1-air.dtb");
const P1_INITRD: &[u8] = include_bytes!("../assets/p1-updater/browser-v1/recovery-initrd.img");
const P1_KERNEL_SHA: &str = "a1bb626b045f799874f6bb98ba99755ae722c15cb3d2fc5c0a45d739bc2994c9";
const P1_DTB_SHA: &str = "03b4b724a4331886ec5b7e15d472104b0763d4af7b6db9ef88be5605f7e3574c";
const P1_INITRD_SHA: &str = "46e3972df53c49ccb32cfe7806ea1920c083b83f2b99387ef4739c6804fa66df";

// The P1 ground units: one recovery, its device tree for any of them.
const P1G_KERNEL: &[u8] = include_bytes!("../assets/p1-gnd-updater/browser-v1/kernel.img");
const P1G_DTB: &[u8] = include_bytes!("../assets/p1-gnd-updater/browser-v1/betafpv-p1-gnd.dtb");
const P1G_INITRD: &[u8] = include_bytes!("../assets/p1-gnd-updater/browser-v1/recovery-initrd.img");
const P1G_KERNEL_SHA: &str = "ab5935c57103459fcf8751bdde068bcd488777aff48b1c925e2e4a77892c9cb7";
const P1G_DTB_SHA: &str = "e95e474ba826ecf2b29b4809343c3667ff1193dedb13c55b358ff64114c23779";
const P1G_INITRD_SHA: &str = "c2e84af285799781395b46363407c2178c93ccc647a2eb09053f7532f517b344";

// What a P1's bootloader is told before `bootm` on the v2 loader, in commands
// of at most 15 characters: keep the DTB/initrd where they were loaded, and
// boot non-secure (what the browser's P1 boot, proven from stock 1.0.44, also
// sets). Names too long for 15 characters are assembled and `run`.
const P1_BOOT_ENV: [&str; 15] = [
    "set H ffffffff", "set H $H$H", "set fdt_high $H",
    "set A setenv", "set i initrd_hi", "set c $A $i", "set c ${c}gh $H", "run c",
    "set B bootm_", "set C boot_", "set D mode", "set E nosec",
    "set c $A $B$C", "set c $c$D $E", "run c",
];

// Where the unit finishes its install after this tool puts it in recovery mode:
// the flasher page that offered this build. A prerelease (a version with a
// pre-release part, 0.5.0-alpha2: the tag `recovery-app-v0.5.0-alpha2`) is the
// preview page's app and sends the unit back to /preview/, which may support
// boards the live page does not yet; a release goes to the live page.
// FPVOS_FLASHER_URL at build time overrides both.
const LIVE_FLASHER_URL: &str = "https://gehee.github.io/fpvOS_flasher/";
const PREVIEW_FLASHER_URL: &str = "https://gehee.github.io/fpvOS_flasher/preview/";
pub const FLASHER_URL: &str = match option_env!("FPVOS_FLASHER_URL") {
    Some(url) => url,
    None if env!("CARGO_PKG_VERSION_PRE").is_empty() => LIVE_FLASHER_URL,
    None => PREVIEW_FLASHER_URL,
};

/// A supported board. The v2 bootloader is identical across all of them; only
/// the stock USB id, the recovery bundle, and the loader frame sizes differ.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Board {
    SteadyDigital,
    P1Air,
    /// The VR04 goggles or the P1 HD VRX (Standard, Nano): one recovery for all of them.
    P1Gnd,
}

impl Board {
    pub fn label(self) -> &'static str {
        match self {
            Board::SteadyDigital => "Skyzone SteadyDigital",
            Board::P1Air => "BETAFPV P1 Air",
            Board::P1Gnd => "BETAFPV VR04 goggles or P1 HD VRX",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Board::SteadyDigital => "steadydigital",
            Board::P1Air => "p1-air",
            Board::P1Gnd => "p1-gnd",
        }
    }
    pub fn from_id(s: &str) -> Option<Board> {
        Board::ALL.into_iter().find(|b| b.id() == s)
    }
    pub const ALL: [Board; 3] = [Board::SteadyDigital, Board::P1Air, Board::P1Gnd];
    /// What a stock P1 (1d6b:0101) may be when its interfaces say nothing.
    pub const STOCK_P1: [Board; 2] = [Board::P1Air, Board::P1Gnd];
    /// The ids, for a usage line.
    pub fn ids(boards: &[Board]) -> String {
        boards.iter().map(|b| b.id()).collect::<Vec<_>>().join(" | ")
    }
    // Stock USB product ids (vendor is always 0x1d6b): SteadyDigital 0x0210,
    // every P1 0x0101. All bootloaders are 0x0100, so a board can only be
    // told apart while still in stock mode (see detect / loader::present).
    //
    // The P1 ground units' U-Boot is the P1 Air's build for build (the vendor's
    // packages differ in the build date and the board device tree), so they
    // are driven as the P1 Air is.
    //
    /// Data bytes per read reply. Writes: see write_chunk - the v2 loader
    /// (P1 stock 2.x, SteadyDigital) commits one 64-byte packet per frame (48
    /// header + 15 data); 960-byte frames are a v1 (stock 1.0.44) loader trait
    /// and corrupt the RAM image on 2.x (CRC mismatch, first P1 2.1.2 run).
    fn read_chunk(self) -> usize {
        match self {
            Board::SteadyDigital => 8,
            Board::P1Air | Board::P1Gnd => 16,
        }
    }
    /// Data bytes per write frame where the board fixes it; None: the
    /// bootloader's own (v1, stock 1.0.44: 960; v2: 15, one 64-byte packet).
    fn write_chunk(self) -> Option<usize> {
        match self {
            Board::SteadyDigital => Some(15),
            Board::P1Air | Board::P1Gnd => None,
        }
    }
    /// The bootloaders the board may have, in probing order (see
    /// Loader::set_candidates): a P1's v1 first, then v2.
    fn bootloaders(self) -> &'static [&'static Layout] {
        match self {
            Board::SteadyDigital => &[&V2],
            Board::P1Air | Board::P1Gnd => &[&V1, &V2],
        }
    }
    fn kernel(self) -> (&'static [u8], &'static str) {
        match self {
            Board::SteadyDigital => (SD_KERNEL, SD_KERNEL_SHA),
            Board::P1Air => (P1_KERNEL, P1_KERNEL_SHA),
            Board::P1Gnd => (P1G_KERNEL, P1G_KERNEL_SHA),
        }
    }
    fn dtb(self) -> (&'static [u8], &'static str) {
        match self {
            Board::SteadyDigital => (SD_DTB, SD_DTB_SHA),
            Board::P1Air => (P1_DTB, P1_DTB_SHA),
            Board::P1Gnd => (P1G_DTB, P1G_DTB_SHA),
        }
    }
    fn initrd(self) -> (&'static [u8], &'static str) {
        match self {
            Board::SteadyDigital => (SD_INITRD, SD_INITRD_SHA),
            Board::P1Air => (P1_INITRD, P1_INITRD_SHA),
            Board::P1Gnd => (P1G_INITRD, P1G_INITRD_SHA),
        }
    }
    /// U-Boot commands sent before `bootm` on the v2 loader, or none to leave
    /// the bootloader's settings. `bootm` hands the kernel U-Boot's bootargs (or
    /// the DTB's), never the kernel's built-in CONFIG_CMDLINE, so the P1 Air
    /// recovery gets its own line here: maxcpus=1 (the SPL parks CPU1 its own
    /// way - the stock 2.x SPL is not where the DT's spin table expects) and
    /// clk_ignore_unused (or the late clock cleanup stops the board after the
    /// USB gadget is up). Built from short variables: the v2 loader takes
    /// 15-character commands. Avoids a/r/f/n/s, which load_image() and boot()
    /// use. The P1 ground recovery kernel forces its own line (the same
    /// settings, CONFIG_CMDLINE_FORCE), so it only needs P1_BOOT_ENV.
    fn boot_commands(self) -> Vec<&'static str> {
        const P1_AIR_BOOTARGS: [&str; 15] = [
            "set w console=", "set y ttyS0,", "set z 1152000", "set k mem=128M",
            "set u rdinit=/", "set v init", "set m maxcpus=1", "set p nr_cpus=1",
            "set q clk_igno", "set t re_unused",
            "set x $w$y$z $k", "set x $x $u$v", "set x $x $m $p", "set x $x $q$t",
            "set bootargs $x",
        ];
        match self {
            Board::SteadyDigital => Vec::new(),
            Board::P1Air => P1_AIR_BOOTARGS.iter().chain(P1_BOOT_ENV.iter()).copied().collect(),
            Board::P1Gnd => P1_BOOT_ENV.to_vec(),
        }
    }
}

/// The stages a recovery goes through, in order, for the window's step list.
/// `Authorize` (macOS's password prompt) happens before the worker runs, and
/// `Driver` (Windows' one-time WinUSB install) only when it is needed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Authorize,
    Reboot,
    Driver,
    Verify,
    Load,
    Boot,
}

impl Step {
    pub fn id(self) -> &'static str {
        match self {
            Step::Authorize => "authorize",
            Step::Reboot => "reboot",
            Step::Driver => "driver",
            Step::Verify => "verify",
            Step::Load => "load",
            Step::Boot => "boot",
        }
    }
    pub fn from_id(s: &str) -> Option<Step> {
        [Step::Authorize, Step::Reboot, Step::Driver, Step::Verify, Step::Load, Step::Boot]
            .into_iter()
            .find(|step| step.id() == s)
    }
}

/// Events emitted as the flow runs.
pub enum Update {
    Status(String),
    /// A stage begins; every earlier one is complete.
    Step(Step),
    /// Loading the recovery images: bytes so far across all of them.
    Progress { name: String, done: usize, total: usize },
    /// Windows: the bootloader is present but bound to this driver, not WinUSB.
    #[allow(dead_code)]
    NeedsWinusb { driver: String },
    Finished { dry_run: bool },
    Failed(String),
    /// macOS: the administrator prompt was dismissed before anything ran.
    Cancelled,
}

/// What compatible hardware is connected, for the UI to display live.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    None,
    /// A board in stock firmware (ready to put into recovery).
    Stock(Board),
    /// A unit in stock firmware that is one of these, alike over USB: the
    /// user says which (a P1 whose interfaces did not say).
    StockOneOf(&'static [Board]),
    /// A unit already in the Artosyn bootloader — board can't be told apart.
    Bootloader,
}

/// Poll what compatible hardware is connected (non-intrusive).
pub fn detect() -> Detected {
    platform::detect()
}

// USB interface classes: CDC control and data (the stock serial port, and RNDIS
// on some systems), wireless controller and miscellaneous (RNDIS as stock
// declares it), still image (MTP).
const CLASS_CDC: u8 = 0x02;
const CLASS_CDC_DATA: u8 = 0x0a;
const CLASS_WIRELESS: u8 = 0xe0;
const CLASS_MISC: u8 = 0xef;
const CLASS_STILL_IMAGE: u8 = 0x06;

/// Which P1 a stock one (1d6b:0101) is, from the classes of its USB
/// interfaces. Every P1 has that id, but stock's ground firmware serves its SD
/// card over MTP (a still-image interface) beside the network and serial
/// ports, and the air unit's does not - in every stock package compared
/// (air 1.0.44 and 2.1.2; ground 1.0.44, 2.0.5, 2.0.6, 2.0.8 and 2.1.4), not
/// yet seen on a unit. So: MTP present, a ground unit (the VR04 goggles or the
/// P1 HD VRX: one recovery for all); only the network and serial interfaces, the
/// air unit; anything else (or nothing readable), the user says.
/// macOS lists every interface, Windows one entry per function (0 where it
/// could not read the class), so the air unit is "the serial port and at
/// least one more, all of them network or serial".
pub fn stock_p1(classes: &[u8]) -> Detected {
    let known = |c: &u8| [CLASS_CDC, CLASS_CDC_DATA, CLASS_WIRELESS, CLASS_MISC].contains(c);
    if classes.contains(&CLASS_STILL_IMAGE) {
        Detected::Stock(Board::P1Gnd)
    } else if classes.len() >= 2 && classes.contains(&CLASS_CDC) && classes.iter().all(known) {
        Detected::Stock(Board::P1Air)
    } else {
        Detected::StockOneOf(&Board::STOCK_P1)
    }
}

/// Whether `board` can be what is connected: a stock unit that says what it
/// is (or what it is one of) is never driven as something else. A unit in the
/// bootloader, or none found, says nothing.
pub fn consistent(board: Board, detected: Detected) -> bool {
    match detected {
        Detected::Stock(b) => b == board,
        Detected::StockOneOf(list) => list.contains(&board),
        Detected::Bootloader | Detected::None => true,
    }
}

/// Whether the current process must relaunch an elevated worker to claim USB
/// (macOS, when not already root). Windows approach A needs no elevation.
pub fn needs_elevation() -> bool {
    platform::needs_elevation()
}

/// A sink for `Update`s. The CLI prints them; the GUI forwards them to its
/// window thread.
pub type Report<'a> = dyn Fn(Update) + 'a;

fn human(bytes: usize) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Load one image; `base`/`grand` place its bytes within the whole load, so
/// progress is one figure across the device tree, initrd and kernel.
fn load(link: &Loader, name: &str, addr: u64, bytes: &[u8], base: usize, grand: usize, report: &Report) -> Result<(), String> {
    report(Update::Status(format!("Loading {name} into RAM ({})…", human(bytes.len()))));
    let crc = crc32(bytes);
    link.load_image(addr, bytes, crc, |done, _total| {
        report(Update::Progress { name: name.to_string(), done: base + done, total: grand });
    })?;
    report(Update::Status(format!("{name}: verified on-device (CRC32 {crc:08x}).")));
    Ok(())
}

fn run_inner(dry_run: bool, board: Board, report: &Report) -> Result<(), String> {
    let (kernel, kernel_sha) = board.kernel();
    let (dtb, dtb_sha) = board.dtb();
    let (initrd, initrd_sha) = board.initrd();
    if sha256(kernel) != kernel_sha || sha256(dtb) != dtb_sha || sha256(initrd) != initrd_sha {
        return Err("Bundled recovery payload failed its integrity check.".into());
    }
    report(Update::Status(format!("{} recovery ready: kernel {} bytes.", board.label(), kernel.len())));

    platform::preflight()?;
    let mut link = Loader::new()?;
    link.set_chunks(board.read_chunk(), board.write_chunk());
    link.set_candidates(board.bootloaders());

    report(Update::Step(Step::Reboot));
    let reboot_sent = platform::enter_loader(report)?;
    report(Update::Status("Waiting for the Artosyn USB download gadget…".into()));
    platform::wait_for_loader(&mut link, reboot_sent, report)?;

    report(Update::Step(Step::Verify));
    report(Update::Status("Verifying the bootloader…".into()));
    link.verify(sha256)?;
    report(Update::Status(format!(
        "Bootloader verified: the {} one (4 fingerprints + dispatch table).",
        link.layout().family
    )));

    report(Update::Step(Step::Load));
    let grand = dtb.len() + initrd.len() + kernel.len();
    load(&link, "device tree", DTB_ADDR, dtb, 0, grand, report)?;
    load(&link, "recovery initrd", INITRD_ADDR, initrd, dtb.len(), grand, report)?;
    load(&link, "recovery kernel", KERNEL_ADDR, kernel, dtb.len() + initrd.len(), grand, report)?;

    if dry_run {
        return Ok(());
    }

    report(Update::Step(Step::Boot));
    // The kernel line is the 2.x loader's need (its SPL, its short commands);
    // the 1.0.44 loader boots as the web flasher boots it, with its own.
    let commands = board.boot_commands();
    if link.layout().short_commands && !commands.is_empty() {
        report(Update::Status("Setting the recovery boot settings…".into()));
        link.commands(&commands)?;
    }
    report(Update::Status("Booting into fpvOS recovery mode…".into()));
    link.boot()?;
    Ok(())
}

/// Run the whole flow, reporting a terminal `Finished`/`Failed` at the end.
/// Returns whether it succeeded (for the CLI's exit code).
pub fn run_recovery(dry_run: bool, board: Board, report: &Report) -> bool {
    match run_inner(dry_run, board, report) {
        Ok(()) => {
            report(Update::Finished { dry_run });
            true
        }
        Err(e) => {
            report(Update::Failed(e));
            false
        }
    }
}

/// Console front end: print the `Update`s as they arrive.
pub fn run_console(dry_run: bool, board: Board) -> bool {
    use std::cell::RefCell;
    let last: RefCell<i32> = RefCell::new(-1);
    let report = |u: Update| match u {
        Update::Status(s) => println!("{s}"),
        Update::Step(_) | Update::Cancelled => {}
        Update::Progress { done, total, .. } => {
            let pct = (done * 100 / total.max(1)) as i32;
            let mut l = last.borrow_mut();
            if pct != *l && pct % 5 == 0 {
                println!("  loading recovery: {pct}%");
                *l = pct;
            }
        }
        Update::NeedsWinusb { driver } => {
            let seen = if driver.is_empty() { "no driver".to_string() } else { format!("the \"{driver}\" driver") };
            println!();
            println!("The bootloader is on USB but bound to {seen}, so libusb cannot claim it.");
            println!("One-time setup with Zadig (https://zadig.akeo.ie):");
            println!("  1. Open Zadig (as administrator).");
            println!("  2. Options → List All Devices, then select \"USB download gadget\" (USB ID 1D6B 0100).");
            println!("  3. Choose the WinUSB driver and click Replace/Install Driver.");
            println!("Waiting for the WinUSB-bound bootloader… (leave the unit connected)");
        }
        Update::Finished { dry_run } => {
            println!();
            if dry_run {
                println!("Dry run complete: bootloader verified and all recovery images loaded and");
                println!("checksummed in RAM. Nothing was booted. Power-cycle the unit to restore stock.");
            } else {
                println!("Done. The unit is now in fpvOS recovery mode and has left the bootloader.");
                println!("Open the fpvOS web flasher — it will take over and install over Web Serial:");
                println!("    {FLASHER_URL}");
                println!("(In Chrome or Edge, click Install and pick the unit's serial port.)");
                println!("If anything looks wrong, power-cycle the unit: nothing was written to flash.");
            }
        }
        Update::Failed(e) => eprintln!("\nERROR: {e}"),
    };
    println!("fpvOS Recovery {}: {} → fpvOS recovery mode", env!("CARGO_PKG_VERSION"), board.label());
    run_recovery(dry_run, board, &report)
}

/// Elevated-worker front end: append each `Update` to `status_path` as a
/// tab-separated line the GUI tails. Formats:
///   S <text> | T <step> | P <name> <done> <total> | W <driver> | D <0|1> | F <text> | C
pub fn run_worker(dry_run: bool, board: Board, status_path: &str) -> bool {
    use std::cell::RefCell;
    use std::io::Write;
    let file = RefCell::new(std::fs::OpenOptions::new().create(true).append(true).open(status_path));
    let write = |line: String| {
        if let Ok(f) = file.borrow_mut().as_mut() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    };
    let report = |u: Update| match u {
        Update::Status(s) => write(format!("S\t{s}")),
        Update::Step(s) => write(format!("T\t{}", s.id())),
        Update::Progress { name, done, total } => write(format!("P\t{name}\t{done}\t{total}")),
        Update::NeedsWinusb { driver } => write(format!("W\t{driver}")),
        Update::Finished { dry_run } => write(format!("D\t{}", if dry_run { 1 } else { 0 })),
        Update::Failed(e) => write(format!("F\t{e}")),
        Update::Cancelled => write("C".into()),
    };
    run_recovery(dry_run, board, &report)
}

/// Parse one worker status line back into an `Update` (used by the GUI).
pub fn parse_line(line: &str) -> Option<Update> {
    let mut it = line.splitn(2, '\t');
    let tag = it.next()?;
    let rest = it.next().unwrap_or("");
    match tag {
        "S" => Some(Update::Status(rest.to_string())),
        "T" => Step::from_id(rest.trim()).map(Update::Step),
        "C" => Some(Update::Cancelled),
        "P" => {
            let mut p = rest.split('\t');
            let name = p.next()?.to_string();
            let done = p.next()?.parse().ok()?;
            let total = p.next()?.parse().ok()?;
            Some(Update::Progress { name, done, total })
        }
        "W" => Some(Update::NeedsWinusb { driver: rest.to_string() }),
        "D" => Some(Update::Finished { dry_run: rest.trim() == "1" }),
        "F" => Some(Update::Failed(rest.to_string())),
        _ => None,
    }
}

/// Open a URL in the default browser (best effort).
pub fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    crate::win::open_url(url);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = url;
}

// ---- Platform: macOS -------------------------------------------------------

#[cfg(target_os = "macos")]
mod platform {
    use super::{Board, Detected, Report, Update};
    use crate::loader::Loader;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    const REBOOT_PACKET: [u8; 9] = [0xaa, 0x05, 0x00, 0x1d, 0x00, 0x00, 0xcc, 0xeb, 0xaa];

    pub fn detect() -> Detected {
        let (p1, sd, boot) = crate::loader::present();
        if sd {
            Detected::Stock(Board::SteadyDigital)
        } else if let Some(classes) = p1 {
            super::stock_p1(&classes)
        } else if boot {
            Detected::Bootloader
        } else {
            Detected::None
        }
    }

    fn is_root() -> bool {
        Command::new("id").arg("-u").output().map(|o| o.stdout == b"0\n").unwrap_or(false)
    }

    pub fn needs_elevation() -> bool {
        !is_root()
    }

    pub fn preflight() -> Result<(), String> {
        if is_root() {
            Ok(())
        } else {
            Err("Administrator access is needed to claim the USB bootloader interface.".into())
        }
    }

    fn usbmodem_ports() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir("/dev") {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("cu.usbmodem") {
                    out.push(e.path());
                }
            }
        }
        out.sort();
        out
    }

    fn reboot(port: &PathBuf) -> Result<(), String> {
        let p = port.to_string_lossy().to_string();
        let st = Command::new("stty")
            .args(["-f", &p, "115200", "cs8", "-parenb", "-cstopb", "raw"])
            .status()
            .map_err(|e| format!("Could not configure {p}: {e}"))?;
        if !st.success() {
            return Err(format!("Could not configure the stock serial port {p}."));
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(port)
            .map_err(|e| format!("Could not open the stock serial port {p}: {e}"))?;
        f.write_all(&REBOOT_PACKET)
            .map_err(|e| format!("Could not send the reboot command on {p}: {e}"))?;
        let _ = f.flush();
        Ok(())
    }

    pub fn enter_loader(report: &Report) -> Result<bool, String> {
        // The reboot packet is the same for both boards; just send it to the one
        // stock serial that is present.
        match usbmodem_ports().as_slice() {
            [] => {
                report(Update::Status("No stock serial port found; assuming the unit is already in the bootloader.".into()));
                Ok(false)
            }
            [port] => {
                report(Update::Status(format!("Rebooting into the bootloader via {}…", port.to_string_lossy())));
                reboot(port)?;
                Ok(true)
            }
            many => {
                let list: Vec<String> = many.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                Err(format!(
                    "Several USB serial ports are present ({}). Disconnect other USB-serial devices and leave only the unit, then retry.",
                    list.join(", ")
                ))
            }
        }
    }

    pub fn wait_for_loader(link: &mut Loader, reboot_sent: bool, report: &Report) -> Result<(), String> {
        let wait = if reboot_sent { Duration::from_secs(30) } else { Duration::from_secs(8) };
        let speed = link.open_wait(wait)?;
        report(Update::Status(format!("Bootloader detected — USB {speed} speed.")));
        Ok(())
    }
}

// ---- Platform: Windows -----------------------------------------------------

#[cfg(target_os = "windows")]
mod platform {
    use super::{Board, Detected, Report, Update};
    use crate::loader::{Loader, Opened};
    use crate::win;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const BOOTLOADER: &str = "VID_1D6B&PID_0100";
    const STOCK_SD: &str = "VID_1D6B&PID_0210";
    const STOCK_P1: &str = "VID_1D6B&PID_0101";
    const REBOOT_PACKET: [u8; 9] = [0xAA, 0x05, 0x00, 0x1D, 0x00, 0x00, 0xCC, 0xEB, 0xAA];

    pub fn detect() -> Detected {
        let devices = win::usb_devices();
        let has = |id: &str| devices.iter().any(|d| d.instance.contains(id));
        if has(STOCK_SD) {
            Detected::Stock(Board::SteadyDigital)
        } else if has(STOCK_P1) {
            super::stock_p1(&win::interface_classes(&devices, STOCK_P1))
        } else if has(BOOTLOADER) {
            Detected::Bootloader
        } else {
            Detected::None
        }
    }

    // The driver bound to the bootloader's USB device, None while it is absent.
    fn loader_driver() -> Option<String> {
        win::usb_devices().into_iter().find(|d| d.instance.contains(BOOTLOADER)).map(|d| d.service)
    }

    pub fn needs_elevation() -> bool {
        false
    }

    pub fn preflight() -> Result<(), String> {
        Ok(())
    }

    // The reboot packet is the same for both boards: send it to whichever
    // stock serial port is present.
    pub fn enter_loader(report: &Report) -> Result<bool, String> {
        let Some(port) = win::com_port(&[STOCK_SD, STOCK_P1]) else {
            report(Update::Status("No stock serial port found; assuming the unit is already in the bootloader.".into()));
            return Ok(false);
        };
        report(Update::Status(format!("Rebooting into the bootloader via {port}…")));
        win::write_serial(&port, &REBOOT_PACKET)
            .map_err(|e| format!("Could not reboot the unit into the bootloader: {e}"))?;
        Ok(true)
    }

    // The fpvos-winusb.exe helper (libwdi, as Zadig uses), embedded at build
    // time; empty when the build had none.
    const EMBEDDED_HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fpvos-winusb.exe"));

    // The app's own folder under %LOCALAPPDATA% (not Temp), for the helper and
    // the driver files it prepares.
    fn app_dir() -> std::path::PathBuf {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("fpvOS Recovery")
    }

    // The embedded helper, written fresh to a per-version file (never a stale
    // copy), else one shipped beside the app.
    fn helper_path() -> Option<std::path::PathBuf> {
        if !EMBEDDED_HELPER.is_empty() {
            let dir = app_dir();
            let path = dir.join(format!("fpvos-winusb-{}.exe", env!("CARGO_PKG_VERSION")));
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, EMBEDDED_HELPER).is_ok() {
                return Some(path);
            }
        }
        let beside = std::env::current_exe().ok()?.with_file_name("fpvos-winusb.exe");
        beside.is_file().then_some(beside)
    }

    // Bind WinUSB to the bootloader's hardware id (USB\VID_1D6B&PID_0100) and
    // nothing else, with the helper (built for the inbox WinUSB of Windows 8
    // and later), run as administrator: one UAC prompt. False when there is no
    // helper, the prompt is declined or the install fails; the caller then
    // falls back to the manual Zadig step.
    fn install_winusb(report: &Report) -> bool {
        let Some(helper) = helper_path() else {
            return false;
        };
        let dest = app_dir().join("driver");
        report(Update::Status("Installing the WinUSB driver for the bootloader — approve the Windows prompt…".into()));
        let args = format!(
            "--vid 0x1D6B --pid 0x0100 --type 0 --name \"fpvOS recovery bootloader\" --manufacturer fpvOS --silent --dest \"{}\"",
            dest.display()
        );
        match win::run_elevated(&helper, &args) {
            win::Elevated::Exited(0) => true,
            win::Elevated::Exited(code) => {
                report(Update::Status(format!("The WinUSB driver install failed (code {code}).")));
                false
            }
            win::Elevated::Declined => {
                report(Update::Status("The driver install was not approved.".into()));
                false
            }
            win::Elevated::Failed(e) => {
                report(Update::Status(format!("Could not start the driver install (error {e}).")));
                false
            }
        }
    }

    // libusb's Windows HID backend opens the gadget under its stock HID driver
    // too, but Windows' HID API rejects the bootloader's undeclared report ids,
    // so every transfer would fail. Only open it once it is bound to WinUSB.
    pub fn wait_for_loader(link: &mut Loader, reboot_sent: bool, report: &Report) -> Result<(), String> {
        // As long as macOS waits: stock shuts down cleanly before it restarts,
        // and a P1 ground unit on stock 2.1.4 was reported not back within 15 s.
        let appear = Instant::now() + Duration::from_secs(if reboot_sent { 30 } else { 5 });
        let svc = loop {
            if let Some(svc) = loader_driver() {
                break svc;
            }
            if Instant::now() >= appear {
                return Err("The bootloader (1d6b:0100) did not appear. Power-cycle the unit and retry.".into());
            }
            sleep(Duration::from_millis(300));
        };
        if !svc.eq_ignore_ascii_case("winusb") {
            report(Update::Step(super::Step::Driver));
        }
        if !svc.eq_ignore_ascii_case("winusb") && install_winusb(report) {
            report(Update::Status("WinUSB driver installed.".into()));
        } else if !svc.eq_ignore_ascii_case("winusb") {
            report(Update::NeedsWinusb { driver: if svc.is_empty() { "no driver".into() } else { svc } });
            let deadline = Instant::now() + Duration::from_secs(300);
            loop {
                sleep(Duration::from_secs(1));
                match loader_driver() {
                    Some(s) if s.eq_ignore_ascii_case("winusb") => break,
                    None => return Err("The bootloader left USB before WinUSB was bound. Power-cycle the unit and retry.".into()),
                    _ if Instant::now() >= deadline => {
                        return Err("Timed out waiting for the WinUSB bind. Complete the Zadig step, then retry.".into())
                    }
                    _ => {}
                }
            }
            report(Update::Status("Bootloader is WinUSB-bound; waiting for it to settle…".into()));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Opened::Yes(speed) = link.try_open() {
                report(Update::Status(format!("Bootloader detected — USB {speed} speed.")));
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("The WinUSB-bound bootloader could not be opened. Power-cycle the unit and retry.".into());
            }
            sleep(Duration::from_millis(500));
        }
    }
}

// ---- Platform: other (unsupported) -----------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::{Detected, Report, Update};
    use crate::loader::Loader;
    pub fn detect() -> Detected {
        Detected::None
    }
    pub fn needs_elevation() -> bool {
        false
    }
    pub fn preflight() -> Result<(), String> {
        Err("This tool supports macOS and Windows. On Linux/ChromeOS, use the fpvOS web flasher instead.".into())
    }
    pub fn enter_loader(_report: &Report) -> Result<bool, String> {
        let _ = Update::Status(String::new());
        Ok(false)
    }
    pub fn wait_for_loader(_link: &mut Loader, _reboot_sent: bool, _report: &Report) -> Result<(), String> {
        Err("unsupported platform".into())
    }
}

// ---- SHA-256 (no external crates) ------------------------------------------

pub fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h = [
        0x6a09e667u32, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(chunk.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut q] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = q.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            q = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e, f, g, q]) {
            *state = state.wrapping_add(value);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

// ---- CRC-32 (zlib/IEEE, matches U-Boot's `crc32`) --------------------------

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prerelease_hands_off_to_the_preview_page_a_release_to_the_live_one() {
        if option_env!("FPVOS_FLASHER_URL").is_some() {
            return;
        }
        let pre = !env!("CARGO_PKG_VERSION_PRE").is_empty();
        assert_eq!(FLASHER_URL, if pre { PREVIEW_FLASHER_URL } else { LIVE_FLASHER_URL });
    }

    fn boards(d: Detected) -> Vec<&'static str> {
        match d {
            Detected::Stock(b) => vec![b.id()],
            Detected::StockOneOf(list) => list.iter().map(|b| b.id()).collect(),
            _ => vec![],
        }
    }

    #[test]
    fn a_stock_p1_is_told_apart_by_its_mtp_interface() {
        // air: RNDIS (control + data) and the serial port (control + data)
        assert_eq!(boards(stock_p1(&[0xe0, 0x0a, 0x02, 0x0a])), ["p1-air"]);
        assert_eq!(boards(stock_p1(&[0xef, 0x0a, 0x02, 0x0a])), ["p1-air"]);
        assert_eq!(boards(stock_p1(&[0x02, 0x0a, 0x02, 0x0a])), ["p1-air"]);
        // ground: the same with MTP between them
        assert_eq!(boards(stock_p1(&[0xe0, 0x0a, 0x06, 0x02, 0x0a])), ["p1-gnd"]);
        // unreadable, or anything not known: the user says
        assert_eq!(boards(stock_p1(&[])), ["p1-air", "p1-gnd"]);
        assert_eq!(boards(stock_p1(&[0xe0, 0x0a, 0xff, 0x02, 0x0a])), ["p1-air", "p1-gnd"]);
        assert_eq!(boards(stock_p1(&[0x02])), ["p1-air", "p1-gnd"]);
        // Windows: one entry per function, 0 for a class it could not read
        assert_eq!(boards(stock_p1(&[0xe0, 0x02])), ["p1-air"]);
        assert_eq!(boards(stock_p1(&[0xe0, 0x06, 0x02])), ["p1-gnd"]);
        assert_eq!(boards(stock_p1(&[0xe0, 0x00, 0x02])), ["p1-air", "p1-gnd"]);
        assert_eq!(boards(stock_p1(&[0xe0, 0x00])), ["p1-air", "p1-gnd"]);
    }

    #[test]
    fn a_stock_unit_is_never_driven_as_another() {
        let ground = stock_p1(&[0xe0, 0x0a, 0x06, 0x02, 0x0a]);
        assert!(consistent(Board::P1Gnd, ground));
        assert!(!consistent(Board::P1Air, ground) && !consistent(Board::SteadyDigital, ground));
        let air = stock_p1(&[0xe0, 0x0a, 0x02, 0x0a]);
        assert!(consistent(Board::P1Air, air) && !consistent(Board::P1Gnd, air));
        assert!(!consistent(Board::P1Air, Detected::Stock(Board::SteadyDigital)));
        // interfaces that said nothing: either P1, never another board
        let unknown = stock_p1(&[]);
        assert!(consistent(Board::P1Air, unknown) && consistent(Board::P1Gnd, unknown));
        assert!(!consistent(Board::SteadyDigital, unknown));
        // the bootloader says nothing: the user's word stands
        for b in Board::ALL {
            assert!(consistent(b, Detected::Bootloader) && consistent(b, Detected::None));
        }
    }

    #[test]
    fn board_ids_round_trip() {
        for b in Board::ALL {
            assert!(Board::from_id(b.id()) == Some(b));
        }
        assert!(Board::from_id("p1").is_none());
        assert!(Board::from_id("p1-goggles").is_none());
    }

    #[test]
    fn the_ground_recovery_names_no_unit() {
        // one tree for any ground unit: the unit tells itself apart once it runs
        let has = |needle: &[u8]| P1G_DTB.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"betafpv,p1-gnd\0"));
        assert!(!has(b"betafpv,p1-goggle") && !has(b"betafpv,p1-nano-vrx"));
    }
}
