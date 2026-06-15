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

/// Press the left button at `from`, drag to `to` (with a few intermediate moves
/// so apps tracking `WM_MOUSEMOVE` register a real drag), and release. Used to
/// drag pane splitters, which expose no UIA element but respond to mouse drags.
///
/// One self-contained press→move→release per call: nothing stays held across
/// calls, so there's no risk of a stuck mouse button on an early exit.
pub fn drag(from: (i32, i32), to: (i32, i32)) {
    /// Max intermediate move points between press and release.
    const MAX_SUBSTEPS: i32 = 8;
    /// Target spacing (px) between intermediate move points.
    const SUBSTEP_PX: i32 = 4;

    // SAFETY: SendInput with a well-formed input array; coords are normalized.
    unsafe {
        let mut inputs: Vec<INPUT> = Vec::new();
        inputs.push(input_at(from.0, from.1, MOUSEEVENTF_MOVE));
        inputs.push(input_at(from.0, from.1, MOUSEEVENTF_LEFTDOWN));

        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let dist = dx.abs().max(dy.abs());
        let steps = (dist / SUBSTEP_PX).clamp(1, MAX_SUBSTEPS);
        for s in 1..steps {
            let x = from.0 + dx * s / steps;
            let y = from.1 + dy * s / steps;
            inputs.push(input_at(x, y, MOUSEEVENTF_MOVE));
        }

        inputs.push(input_at(to.0, to.1, MOUSEEVENTF_MOVE));
        inputs.push(input_at(to.0, to.1, MOUSEEVENTF_LEFTUP));
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Move to `(x, y)` on the virtual desktop and issue a `down`/`up` button pair.
fn send_at(x: i32, y: i32, down: MOUSE_EVENT_FLAGS, up: MOUSE_EVENT_FLAGS) {
    // SAFETY: SendInput with a well-formed input array; coords are normalized.
    unsafe {
        let inputs = [
            input_at(x, y, MOUSEEVENTF_MOVE),
            input_at(x, y, down),
            input_at(x, y, up),
        ];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Build a single mouse `INPUT` at physical point `(x, y)` carrying `flags`
/// (plus absolute/virtual-desktop positioning).
unsafe fn input_at(x: i32, y: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    let (ax, ay) = normalize(x, y);
    INPUT {
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
    }
}

/// Normalize a physical screen point to the 0..=65535 absolute range over the
/// whole virtual desktop (all monitors), per the `SendInput` contract.
unsafe fn normalize(x: i32, y: i32) -> (i32, i32) {
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
    let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
    let ax = ((x - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
    let ay = ((y - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;
    (ax, ay)
}
