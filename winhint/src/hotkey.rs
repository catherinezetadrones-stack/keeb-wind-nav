//! Global low-level keyboard hook (`WH_KEYBOARD_LL`).
//!
//! The hook callback must stay fast — Windows silently drops a low-level hook
//! whose owning thread exceeds `LowLevelHooksTimeout`. So the hook lives on its
//! own dedicated thread (see `install`) that does nothing but pump messages,
//! and the proc itself does the minimum: detect the trigger / hint keys,
//! `PostMessage` an intent to the app window, and suppress keys (return 1) so
//! they don't leak into the focused app while hint mode is active. All real work
//! (scan, filter, click) happens in the window proc on the app thread, which the
//! hook thread never waits on.
//!
//! Trigger: **tap CapsLock** to enter hint mode; tap it again (or Esc) to
//! leave (suppressed, so it never toggles Caps).
//! Quit chord: Ctrl+Alt+Q (idle only).
//!
//! In `--debug`, every key the hook sees is logged to stderr — the fastest way
//! to confirm the hook is alive and see the real virtual-key codes.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MENU,
    VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::app::{
    WM_APP_CANCEL, WM_APP_CONFIRM, WM_APP_KEY, WM_APP_NAV, WM_APP_NAV_H, WM_APP_QUIT, WM_APP_RESIZE,
    WM_APP_SHOW, WM_APP_TAB,
};

/// Virtual-key code for the `Q` key (no named constant in the windows crate).
const VK_Q: u32 = 0x51;
/// Low-level hook flag: the key is an extended key (e.g. Right Ctrl/Alt).
const LLKHF_EXTENDED: u32 = 0x01;
/// Max gap (ms) between two CapsLock taps to count as a double-tap → resize mode.
/// Compared against `KBDLLHOOKSTRUCT.time`, which is a millisecond tick count.
const DOUBLE_TAP_MS: u32 = 400;

/// The app window to post intents to (HWND pointer as usize), whether hint mode
/// is active, and whether to log keys. `HOOK_THREAD_ID` is the dedicated hook
/// thread's id, used to signal it to stop.
static HOOK_TARGET: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DEBUG: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// True while resize mode is active (a sub-state of `ACTIVE`). Changes how the
/// hook routes CapsLock (always exits, never re-triggers) and Left/Right arrows.
static RESIZE_ACTIVE: AtomicBool = AtomicBool::new(false);
/// `KBDLLHOOKSTRUCT.time` of the CapsLock-down that entered hint mode, used to
/// detect a quick second tap (→ resize). Only meaningful while `ACTIVE`.
static LAST_CAPS_TIME: AtomicU32 = AtomicU32::new(0);

/// Install the keyboard hook on a dedicated thread, posting intents to `hwnd`.
///
/// A `WH_KEYBOARD_LL` callback runs on the thread that installed the hook, and
/// that thread must keep pumping messages — Windows silently bypasses a hook
/// whose owning thread doesn't respond within `LowLevelHooksTimeout`, letting
/// the keystroke (e.g. a CapsLock toggle) leak through. The app thread can stall
/// for that long during a UIA scan, so the hook lives on its own thread that
/// does nothing but pump. Intents reach the app via cross-thread `PostMessageW`.
///
/// Blocks until the hook is installed (or installation fails) on that thread.
pub fn install(hwnd: HWND) -> Result<()> {
    HOOK_TARGET.store(hwnd.0 as usize, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("winhint-hook".into())
        .spawn(move || {
            // SAFETY: standard hook install + message pump, confined to this
            // dedicated thread; the HHOOK never leaves it.
            unsafe {
                let installed = GetModuleHandleW(None)
                    .map_err(|e| e.to_string())
                    .and_then(|hmod| {
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod.into()), 0)
                            .map_err(|e| e.to_string())
                    });
                let hook = match installed {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                // Record our id (SetWindowsHookExW already created this thread's
                // message queue) so `uninstall` can post us WM_QUIT, then signal
                // the installer that we're live.
                HOOK_THREAD_ID.store(GetCurrentThreadId(), Ordering::Relaxed);
                let _ = tx.send(Ok(()));

                // Tight pump: nothing heavy here, so Windows can always invoke
                // `keyboard_proc` in time. Exits when `uninstall` posts WM_QUIT.
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = UnhookWindowsHookEx(hook);
            }
        })
        .map_err(|e| anyhow!("spawning hook thread failed: {e}"))?;

    match rx.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(anyhow!("SetWindowsHookExW failed: {e}")),
        Err(_) => Err(anyhow!("hook thread exited before installing the hook")),
    }
}

/// Stop the hook thread: post it WM_QUIT, so it unhooks and exits its loop.
pub fn uninstall() {
    let tid = HOOK_THREAD_ID.swap(0, Ordering::Relaxed);
    if tid != 0 {
        // SAFETY: posting WM_QUIT to a thread's message queue is always safe;
        // a stale id just means the post is ignored.
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// Tell the hook whether hint mode is active (changes which keys it captures).
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// Tell the hook whether resize mode is active (a sub-state of active).
pub fn set_resize_active(resize: bool) {
    RESIZE_ACTIVE.store(resize, Ordering::Relaxed);
}

/// Enable per-key logging to stderr (for diagnosing the hook).
pub fn set_debug(debug: bool) {
    DEBUG.store(debug, Ordering::Relaxed);
}

/// Is a virtual key currently pressed?
unsafe fn key_down(vk: u16) -> bool {
    (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0
}

/// Are both Ctrl and Alt held right now?
unsafe fn ctrl_alt_held() -> bool {
    key_down(VK_CONTROL.0) && key_down(VK_MENU.0)
}

/// Is Shift held right now? (Shift is never suppressed, so we read it live when
/// posting an intent — used to choose right-click vs left-click.)
unsafe fn shift_held() -> bool {
    key_down(VK_SHIFT.0)
}

/// True for modifier keys (never suppressed, to avoid a stuck modifier).
fn is_modifier(vk: u32) -> bool {
    vk == VK_CONTROL.0 as u32
        || vk == VK_MENU.0 as u32
        || vk == VK_SHIFT.0 as u32
        || vk == VK_LWIN.0 as u32
        || vk == VK_RWIN.0 as u32
        || (0xA0..=0xA5).contains(&vk) // L/R Ctrl/Shift/Alt variants
}

/// Is this a text key the search query / hint code accepts (a-z, 0-9, Space,
/// or Backspace)? These are forwarded to the app as `WM_APP_KEY`.
fn is_text_key(vk: u32) -> bool {
    (0x41..=0x5A).contains(&vk) // a-z
        || (0x30..=0x39).contains(&vk) // 0-9
        || vk == VK_SPACE.0 as u32
        || vk == VK_BACK.0 as u32
}

unsafe fn post(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) {
    let _ = PostMessageW(Some(hwnd), msg, WPARAM(wparam), LPARAM(lparam));
}

/// The hook callback. Runs on the dedicated hook thread (see `install`); it only
/// reads atomics and `PostMessageW`s to the app window — never touches app state.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode;
        let m = wparam.0 as u32;
        let is_down = m == WM_KEYDOWN || m == WM_SYSKEYDOWN;
        let is_up = m == WM_KEYUP || m == WM_SYSKEYUP;
        let extended = (kb.flags.0 & LLKHF_EXTENDED) != 0;
        let active = ACTIVE.load(Ordering::Relaxed);

        if DEBUG.load(Ordering::Relaxed) && (is_down || is_up) {
            eprintln!(
                "[hook] {} vk=0x{:02X} ext={} active={}",
                if is_down { "DOWN" } else { "UP  " },
                vk,
                extended as u8,
                active as u8
            );
        }

        let hwnd = HWND(HOOK_TARGET.load(Ordering::Relaxed) as *mut _);
        if !hwnd.0.is_null() {
            if !active {
                // --- Idle: CapsLock activates; Ctrl+Alt+Q quits. ---
                if is_down {
                    if vk == VK_CAPITAL.0 as u32 {
                        // Normally the first tap enters hint mode (and we record
                        // its time so a second tap can be recognised as a
                        // double-tap → resize). But a slow UIA scan can hold off
                        // the ACTIVE flip past a quick second tap, so that second
                        // CapsLock still lands here in the idle branch. Detect the
                        // double-tap here too and post WM_APP_RESIZE: the app
                        // applies it right after the queued WM_APP_SHOW has put it
                        // into hint mode (enter_resize guards on that state).
                        if kb.time.wrapping_sub(LAST_CAPS_TIME.load(Ordering::Relaxed))
                            <= DOUBLE_TAP_MS
                        {
                            post(hwnd, WM_APP_RESIZE, 0, 0);
                        } else {
                            LAST_CAPS_TIME.store(kb.time, Ordering::Relaxed);
                            post(hwnd, WM_APP_SHOW, 0, 0);
                        }
                        return LRESULT(1); // suppress the Caps toggle
                    }
                    if vk == VK_Q && ctrl_alt_held() {
                        post(hwnd, WM_APP_QUIT, 0, 0);
                        return LRESULT(1);
                    }
                } else if is_up && vk == VK_CAPITAL.0 as u32 {
                    return LRESULT(1); // suppress the matching key-up too
                }
            } else {
                // --- Active: route text/Enter/Tab keys; swallow other
                //     non-modifier keys. Shift (bit 0 of lparam) selects
                //     right-click vs left-click for Enter / hint commit. ---
                let resize = RESIZE_ACTIVE.load(Ordering::Relaxed);
                if is_down {
                    // Esc always exits (in resize mode it restores the original
                    // rect; in hint mode it just cancels — the app decides).
                    if vk == VK_ESCAPE.0 as u32 {
                        post(hwnd, WM_APP_CANCEL, 0, 0);
                        return LRESULT(1);
                    }
                    // CapsLock: in resize mode it exits; in hint mode a quick
                    // second tap (within DOUBLE_TAP_MS of the activating tap)
                    // upgrades to resize mode, otherwise it cancels.
                    if vk == VK_CAPITAL.0 as u32 {
                        if !resize
                            && kb.time.wrapping_sub(LAST_CAPS_TIME.load(Ordering::Relaxed))
                                <= DOUBLE_TAP_MS
                        {
                            post(hwnd, WM_APP_RESIZE, 0, 0);
                        } else {
                            post(hwnd, WM_APP_CANCEL, 0, 0);
                        }
                        return LRESULT(1);
                    }
                    if vk == VK_RETURN.0 as u32 {
                        post(hwnd, WM_APP_CONFIRM, 0, shift_held() as isize);
                        return LRESULT(1);
                    }
                    if vk == VK_TAB.0 as u32 {
                        // Tab cycles match mode in hint mode; no-op (swallowed) in
                        // resize mode.
                        if !resize {
                            post(hwnd, WM_APP_TAB, 0, 0);
                        }
                        return LRESULT(1);
                    }
                    // Arrow Up/Down: move the list selection (hint mode) or a
                    // handle's edge vertically (resize mode). Shift → fine step.
                    if vk == VK_UP.0 as u32 {
                        post(hwnd, WM_APP_NAV, 0, shift_held() as isize);
                        return LRESULT(1);
                    }
                    if vk == VK_DOWN.0 as u32 {
                        post(hwnd, WM_APP_NAV, 1, shift_held() as isize);
                        return LRESULT(1);
                    }
                    // Arrow Left/Right: move a handle's edge horizontally in
                    // resize mode (Shift → fine step). Ignored (but swallowed) in
                    // hint mode by the catch-all below.
                    if resize && vk == VK_LEFT.0 as u32 {
                        post(hwnd, WM_APP_NAV_H, 0, shift_held() as isize);
                        return LRESULT(1);
                    }
                    if resize && vk == VK_RIGHT.0 as u32 {
                        post(hwnd, WM_APP_NAV_H, 1, shift_held() as isize);
                        return LRESULT(1);
                    }
                    if is_text_key(vk) {
                        post(hwnd, WM_APP_KEY, vk as usize, shift_held() as isize);
                        return LRESULT(1);
                    }
                    if !is_modifier(vk) {
                        return LRESULT(1);
                    }
                } else if is_up && !is_modifier(vk) {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
