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
//!   winhint --tree [n]   dump the raw UIA tree of the foreground window
//!   winhint --resizables [n]
//!                        list elements that expose the UIA Transform pattern
//!                        (CanResize/CanMove) — the candidate "levels" for nested
//!                        resize beyond the top-level window

mod app;
mod click;
mod hints;
mod hotkey;
mod overlay;
mod resize;
mod scanner;
mod splitter;
mod tray;

use anyhow::Result;
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
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
    let tree = args.iter().any(|a| a == "--tree");
    let resizables = args.iter().any(|a| a == "--resizables");
    let debug = args.iter().any(|a| a == "--debug");
    let delay: u64 = args.iter().find_map(|a| a.parse::<u64>().ok()).unwrap_or(0);

    // WebView2 requires an STA (single-threaded apartment) UI thread; UIA is
    // happy in STA too. Initialize once for this thread.
    // SAFETY: balanced with CoUninitialize before return.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }

    let result = run(scan_only, tree, resizables, debug, delay);

    // SAFETY: matches the CoInitializeEx above on this thread.
    unsafe {
        CoUninitialize();
    }
    result
}

fn run(scan_only: bool, tree: bool, resizables: bool, debug: bool, delay: u64) -> Result<()> {
    if scan_only || tree || resizables {
        if delay > 0 {
            for n in (1..=delay).rev() {
                eprintln!("Scanning foreground window in {n}s — focus your target...");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        if tree {
            scanner::dump_tree()?;
            return Ok(());
        }
        if resizables {
            scanner::dump_resizables()?;
            return Ok(());
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

    // Single-instance guard (daemon mode only — `--scan` is a one-shot that
    // installs no hook, so it may run alongside a daemon). A second daemon would
    // install a second global CapsLock hook and the two would fight over every
    // keystroke, so refuse to start when one is already running. This is the
    // desktop analog of a "port already in use" error.
    let _instance = match SingleInstance::acquire() {
        Some(guard) => guard,
        None => {
            eprintln!("WinHint is already running — only one instance can run at a time.");
            return Ok(());
        }
    };

    app::run(debug)
}

/// Holds the single-instance named mutex for the lifetime of the daemon.
/// Dropping it (on process exit) releases the mutex so the next launch succeeds.
struct SingleInstance(HANDLE);

impl SingleInstance {
    /// Try to become the one running WinHint. Returns `None` when another
    /// instance already owns the named mutex.
    ///
    /// The `Local\` prefix scopes the mutex to the current user session — one
    /// WinHint per desktop, which is what we want (a different logged-in user
    /// gets their own).
    fn acquire() -> Option<Self> {
        // SAFETY: CreateMutexW with a static, session-local name. GetLastError is
        // read immediately after, before any other call can reset it.
        unsafe {
            let handle = match CreateMutexW(None, true, w!("Local\\WinHint_SingleInstance")) {
                Ok(h) => h,
                // Couldn't create the mutex (resource exhaustion, etc.). Don't
                // block startup over a guard we failed to establish; hold a null
                // handle so the rest of the flow is unchanged.
                Err(_) => return Some(SingleInstance(HANDLE::default())),
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(SingleInstance(handle))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a handle from CreateMutexW (or null on the failed-
        // create path, where CloseHandle is a harmless no-op). Closing it
        // releases the mutex so a subsequent launch can acquire it.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
