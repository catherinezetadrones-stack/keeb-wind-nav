//! WinHint — keyboard-driven hint-mode navigation for Windows.
//!
//! Milestones: M1 (UIA scan) · M2 (transparent WebView2 overlay) ·
//! M4 (global hotkey + filtering + click).
//!
//! Usage:
//!   winhint              run as a daemon: tap CapsLock to activate hint mode,
//!                        type a label to click, Esc cancels, Ctrl+Alt+Q quits
//!   winhint --debug      same, but the overlay surface is tinted red for testing
//!   winhint --scan [n]   scan the foreground window and print results (M1 mode);
//!                        optional countdown `n` (secs) to focus a target first

mod app;
mod click;
mod hints;
mod hotkey;
mod overlay;
mod scanner;

use anyhow::Result;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() -> Result<()> {
    // Per-monitor DPI awareness: UIA rects and Win32 window coords are then both
    // in physical pixels, so they line up. Must run before any window/UIA work.
    // SAFETY: simple process-wide flag; failure here is non-fatal.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let scan_only = args.iter().any(|a| a == "--scan");
    let debug = args.iter().any(|a| a == "--debug");
    let delay: u64 = args.iter().find_map(|a| a.parse::<u64>().ok()).unwrap_or(0);

    // WebView2 requires an STA (single-threaded apartment) UI thread; UIA is
    // happy in STA too. Initialize once for this thread.
    // SAFETY: balanced with CoUninitialize before return.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }

    let result = run(scan_only, debug, delay);

    // SAFETY: matches the CoInitializeEx above on this thread.
    unsafe {
        CoUninitialize();
    }
    result
}

fn run(scan_only: bool, debug: bool, delay: u64) -> Result<()> {
    if scan_only {
        if delay > 0 {
            for n in (1..=delay).rev() {
                eprintln!("Scanning foreground window in {n}s — focus your target...");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        let hints = scanner::scan_foreground()?;
        println!("Found {} clickable element(s):\n", hints.len());
        for (i, h) in hints.iter().enumerate() {
            println!(
                "{:>3}  ({:>5},{:>5})  {:<12}  {}",
                i + 1,
                h.cx,
                h.cy,
                h.control,
                h.name
            );
        }
        return Ok(());
    }

    app::run(debug)
}
