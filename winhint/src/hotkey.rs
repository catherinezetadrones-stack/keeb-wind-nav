//! Global low-level keyboard hook (`WH_KEYBOARD_LL`).
//!
//! The hook callback must stay fast — Windows silently drops a low-level hook
//! whose callback exceeds `LowLevelHooksTimeout`. So this proc does the minimum:
//! detect the trigger / hint keys, `PostMessage` an intent to the app window,
//! and suppress keys (return 1) so they don't leak into the focused app while
//! hint mode is active. All real work (scan, filter, click) happens in the
//! window proc, off the hook.
//!
//! Trigger: **tap CapsLock** to enter hint mode; tap it again (or Esc) to
//! leave (suppressed, so it never toggles Caps).
//! Quit chord: Ctrl+Alt+Q (idle only).
//!
//! In `--debug`, every key the hook sees is logged to stderr — the fastest way
//! to confirm the hook is alive and see the real virtual-key codes.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_MENU,
    VK_RETURN, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::app::{
    WM_APP_CANCEL, WM_APP_CONFIRM, WM_APP_KEY, WM_APP_NAV, WM_APP_QUIT, WM_APP_SHOW, WM_APP_TAB,
};

/// Virtual-key code for the `Q` key (no named constant in the windows crate).
const VK_Q: u32 = 0x51;
/// Low-level hook flag: the key is an extended key (e.g. Right Ctrl/Alt).
const LLKHF_EXTENDED: u32 = 0x01;

/// The app window to post intents to (HWND pointer as usize), whether hint mode
/// is active, and whether to log keys. Written only from the app thread.
static HOOK_TARGET: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DEBUG: AtomicBool = AtomicBool::new(false);

/// Install the keyboard hook, posting intents to `hwnd`.
pub fn install(hwnd: HWND) -> Result<HHOOK> {
    HOOK_TARGET.store(hwnd.0 as usize, Ordering::Relaxed);
    // SAFETY: standard hook install; module handle is valid for this process.
    unsafe {
        let hmod: HMODULE = GetModuleHandleW(None)?;
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod.into()), 0)
            .map_err(|e| anyhow!("SetWindowsHookExW failed: {e}"))
    }
}

/// Remove the hook.
pub fn uninstall(hook: HHOOK) {
    // SAFETY: `hook` came from a successful install.
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
}

/// Tell the hook whether hint mode is active (changes which keys it captures).
pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
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

/// The hook callback. Runs on the app thread during message processing.
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
                        post(hwnd, WM_APP_SHOW, 0, 0);
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
                if is_down {
                    // CapsLock doubles as the cancel key (symmetric with the
                    // trigger), alongside Esc. Either one exits hint mode.
                    if vk == VK_ESCAPE.0 as u32 || vk == VK_CAPITAL.0 as u32 {
                        post(hwnd, WM_APP_CANCEL, 0, 0);
                        return LRESULT(1);
                    }
                    if vk == VK_RETURN.0 as u32 {
                        post(hwnd, WM_APP_CONFIRM, 0, shift_held() as isize);
                        return LRESULT(1);
                    }
                    if vk == VK_TAB.0 as u32 {
                        post(hwnd, WM_APP_TAB, 0, 0);
                        return LRESULT(1);
                    }
                    // Arrow Up/Down move the list selection (wparam: 0=up, 1=down).
                    if vk == VK_UP.0 as u32 {
                        post(hwnd, WM_APP_NAV, 0, 0);
                        return LRESULT(1);
                    }
                    if vk == VK_DOWN.0 as u32 {
                        post(hwnd, WM_APP_NAV, 1, 0);
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
