//! fpvOS Recovery: puts an fpvOS-compatible device into fpvOS recovery mode.
//!
//! Boots a stock unit (Skyzone SteadyDigital, BETAFPV P1 Air) into fpvOS
//! recovery mode so the fpvOS web
//! flasher can take over and install over USB. By default it opens a native
//! window (`gui`); `--cli` runs the same flow in the terminal. The recovery
//! logic lives in `engine`, which drives the `loader` (libusb) and reports
//! progress so either front end can show it.
//!
//! On macOS the window runs as the normal user and launches an elevated
//! `--worker` for the USB step (one admin prompt), streaming status back
//! through `--status <file>`. On Windows the app runs as the user; the first
//! time, a helper binds WinUSB to the bootloader (one UAC prompt). Everything lives in RAM: a power cycle restores stock.

// A GUI program on Windows: no console window opens with the app. `--cli`
// and `--help` attach to the console of the terminal that started them.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod engine;
mod gui;
mod loader;
mod shot;
#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
fn attach_console() {
    extern "system" {
        fn AttachConsole(process: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(target_os = "windows"))]
fn attach_console() {}

fn print_help() {
    println!(
        "fpvOS Recovery: put an fpvOS-compatible device into fpvOS recovery mode\n\n\
         Usage: fpvos-recovery [--cli] [--dry-run]\n\n\
         Default opens a window; --cli runs in the terminal. It reboots the unit into the\n\
         Artosyn bootloader, loads fpvOS recovery into RAM, and boots into recovery mode,\n\
         then hand off to the web flasher: https://gehee.github.io/fpvOS_flasher/\n\n\
         macOS:   it prompts for administrator access when it needs to claim USB.\n\
         Windows: the first time, it installs the WinUSB driver for the bootloader\n\
                  (1D6B 0100) after one Windows prompt; Zadig is the fallback.\n\n\
         --cli       run in the terminal instead of opening a window.\n\
         --dry-run   enter the bootloader, verify it, load and on-device checksum the\n\
                     recovery images, but do NOT boot. Power-cycle afterwards.\n\
         A power cycle always restores stock; nothing is written to flash."
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--cli" || a == "-h" || a == "--help") {
        attach_console();
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let mut cli = false;
    let mut dry_run = false;
    let mut worker = false;
    let mut status: Option<String> = None;
    let mut board_id: Option<String> = None;
    let mut demo: Option<String> = None;
    let mut screenshot: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cli" => cli = true,
            // Hidden design aids: a screen with made-up data, and saving the
            // window to a PNG once drawn (then quitting).
            "--demo" => demo = it.next().cloned(),
            "--screenshot" => screenshot = it.next().cloned(),
            "--dry-run" => dry_run = true,
            "--worker" => worker = true,
            "--status" => {
                status = it.next().cloned();
                if status.is_none() {
                    eprintln!("--status needs a file path");
                    std::process::exit(2);
                }
            }
            "--board" => {
                board_id = it.next().cloned();
                if board_id.is_none() {
                    eprintln!("--board needs a value (p1-air | steadydigital)");
                    std::process::exit(2);
                }
            }
            other => {
                eprintln!("Unknown option: {other} (use --help)");
                std::process::exit(2);
            }
        }
    }

    let board = match &board_id {
        Some(id) => match engine::Board::from_id(id) {
            Some(b) => Some(b),
            None => {
                eprintln!("Unknown board '{id}' (use p1-air or steadydigital)");
                std::process::exit(2);
            }
        },
        None => None,
    };

    // Hidden: the elevated helper the GUI launches to do the USB work.
    if worker {
        let (Some(path), Some(board)) = (status, board) else {
            eprintln!("--worker requires --status <file> and --board <id>");
            std::process::exit(2);
        };
        std::process::exit(if engine::run_worker(dry_run, board, &path) { 0 } else { 1 });
    }

    if cli {
        // Use the given board, else whatever is connected in stock.
        let board = board.or_else(|| match engine::detect() {
            engine::Detected::Stock(b) => Some(b),
            _ => None,
        });
        let Some(board) = board else {
            eprintln!("No stock device detected. Connect one, or pass --board p1-air | steadydigital.");
            std::process::exit(2);
        };
        if !engine::run_console(dry_run, board) {
            std::process::exit(1);
        }
    } else if let Err(e) = gui::run(gui::Options { dry_run, demo, screenshot }) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}
