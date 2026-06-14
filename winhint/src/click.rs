//! Simulated mouse clicks via `SendInput`.
//!
//! `mouse_event` (used by the prototype) is deprecated; `SendInput` is the
//! supported path. Absolute coordinates are normalized to 0..=65535 across the
//! whole virtual desktop (all monitors), per the Win32 contract.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSE_EVENT_FLAGS, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Move the cursor to the physical screen point `(x, y)` and left-click it.
pub fn click(x: i32, y: i32) {
    send_at(x, y, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP);
}

/// Move the cursor to the physical screen point `(x, y)` and right-click it
/// (opens the element's context menu).
pub fn right_click(x: i32, y: i32) {
    send_at(x, y, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP);
}

/// Move to `(x, y)` on the virtual desktop and issue a `down`/`up` button pair.
fn send_at(x: i32, y: i32, down: MOUSE_EVENT_FLAGS, up: MOUSE_EVENT_FLAGS) {
    // SAFETY: GetSystemMetrics + SendInput with a well-formed input array.
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);

        // Normalize to the 0..=65535 absolute range over the virtual desktop.
        let ax = ((x - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
        let ay = ((y - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;

        let mk = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: ax,
                    dy: ay,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        let inputs = [mk(MOUSEEVENTF_MOVE), mk(down), mk(up)];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}
