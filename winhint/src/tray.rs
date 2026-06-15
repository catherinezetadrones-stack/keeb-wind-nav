//! System-tray (notification-area) icon + right-click context menu.
//!
//! Adds a single icon via `Shell_NotifyIconW`, routing its mouse events to the
//! app's overlay window as `WM_APP_TRAY`; the app's wndproc calls `show_menu` on
//! a right-click. The menu offers Pause/Resume (toggles the keyboard hook) and
//! Quit. The icon is removed on shutdown (`NIM_DELETE`) so no ghost lingers.
//!
//! A stock Windows icon is used for now (a custom `.ico` is a deferred follow-up
//! — see `TODO.md`).

use anyhow::{anyhow, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_RIGHTBUTTON,
    WM_NULL,
};

use crate::app::WM_APP_TRAY;

/// Fixed id for our single tray icon (any stable value; we only have one).
const TRAY_ICON_ID: u32 = 1;

/// Menu command ids, matched in the app's `WM_COMMAND` handler. Non-zero so they
/// are unambiguous in the `WM_COMMAND` low word.
pub const IDM_PAUSE_RESUME: u32 = 1;
pub const IDM_QUIT: u32 = 2;

/// The identity fields of our icon's `NOTIFYICONDATAW` (hWnd + uID). `add` fills
/// in the visual fields on top of this; `remove` needs only the identity.
fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        ..Default::default()
    }
}

/// Add the tray icon for `hwnd`, routing its mouse events to `WM_APP_TRAY`.
pub fn add(hwnd: HWND) -> Result<()> {
    // SAFETY: LoadIconW with a stock icon name, then Shell_NotifyIconW with a
    // fully-sized, well-formed NOTIFYICONDATAW.
    unsafe {
        let mut data = icon_data(hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = LoadIconW(None, IDI_APPLICATION)?;
        write_tip(&mut data.szTip, "WinHint — tap CapsLock to navigate");
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            return Err(anyhow!("Shell_NotifyIconW(NIM_ADD) failed"));
        }
    }
    Ok(())
}

/// Remove the tray icon (`NIM_DELETE`). Best-effort: failure is ignored since
/// this only runs at shutdown. Safe to call even if `add` never succeeded.
pub fn remove(hwnd: HWND) {
    // SAFETY: identity-only NOTIFYICONDATAW; result intentionally ignored.
    unsafe {
        let data = icon_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// Show the right-click context menu at the cursor. `paused` chooses the
/// Pause/Resume label. Selections come back to `hwnd` as `WM_COMMAND` carrying
/// `IDM_PAUSE_RESUME` / `IDM_QUIT`.
///
/// # Safety
/// `hwnd` must be the live overlay window; called from its wndproc on the app
/// thread.
pub unsafe fn show_menu(hwnd: HWND, paused: bool) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[winhint] tray: CreatePopupMenu failed: {e}");
            return;
        }
    };

    let toggle = if paused { w!("Resume") } else { w!("Pause") };
    let _ = AppendMenuW(menu, MF_STRING, IDM_PAUSE_RESUME as usize, toggle);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, IDM_QUIT as usize, w!("Quit"));

    // Classic tray-menu dance: the owner must be foreground or the menu won't
    // dismiss on an outside click, and a trailing post makes the first click
    // after dismissal register. Our window is WS_EX_NOACTIVATE, so
    // SetForegroundWindow may be refused — verified by manual test.
    let _ = SetForegroundWindow(hwnd);
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
}

/// Copy `s` into a fixed UTF-16 tooltip buffer, NUL-terminated and truncated to
/// fit (`szTip` is 128 wide chars).
fn write_tip(buf: &mut [u16], s: &str) {
    let max = buf.len().saturating_sub(1); // leave room for the NUL
    let utf16: Vec<u16> = s.encode_utf16().take(max).collect();
    buf[..utf16.len()].copy_from_slice(&utf16);
    buf[utf16.len()] = 0;
}
