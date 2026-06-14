//! UIA tree walk of the foreground window plus the taskbar notification area.
//!
//! Collects every element whose control type is in the "clickable" set, with
//! its center point (physical pixels). Filtering rules (depth cap, element cap,
//! off-screen skip, window-rect bounds, pixel dedup) mirror the proven Python
//! prototype (`prototype/winhint.py`).
//!
//! Besides the focused app window, the taskbar's notification area
//! (`Shell_TrayWnd`) is scanned too: tray icons / clock / volume live in that
//! separate top-level window, so walking the app window alone never sees them.
//!
//! COM must already be initialized on the calling thread (see `main`).

// The windows crate's UIA_*ControlTypeId constants are mixed-case; using them
// as match patterns trips the upper-case-globals lint. They are real consts.
#![allow(non_upper_case_globals)]

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
    UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId,
    UIA_DataItemControlTypeId, UIA_EditControlTypeId, UIA_HeaderItemControlTypeId,
    UIA_HyperlinkControlTypeId, UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId,
    UIA_RadioButtonControlTypeId, UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId,
    UIA_TreeItemControlTypeId, UIA_CONTROLTYPE_ID,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetClassNameW, GetForegroundWindow, GetWindowRect,
};

/// A clickable element discovered during the scan.
pub struct Hint {
    /// Center X in physical screen pixels (click target).
    pub cx: i32,
    /// Center Y in physical screen pixels (click target).
    pub cy: i32,
    /// Top edge Y in physical screen pixels — where an above-anchored label sits.
    pub top: i32,
    /// UIA element name (may be empty).
    pub name: String,
    /// Short control-type label, e.g. "Button".
    pub control: &'static str,
    /// Render the hint label *above* the element instead of centered on it.
    /// Set for taskbar/tray icons, which are small and get covered otherwise.
    pub above: bool,
}

/// Maximum tree depth to descend. Matches the prototype's proven value.
const MAX_DEPTH: usize = 12;
/// Cap on collected elements — keeps the scan snappy on dense windows. Shared
/// across all scanned host windows (foreground is walked first), so this is set
/// high enough that a busy app window won't starve the later taskbar scan.
const MAX_ELEMENTS: usize = 300;
/// Two centers within this many pixels (each axis) are treated as the same hit.
const DEDUP_PX: i32 = 4;

/// Scan the foreground window plus the taskbar notification area. Returns an
/// empty vec if there is no foreground window and no taskbar.
///
/// Host HWNDs are captured at call time, so call this *before* showing any
/// overlay (which would itself become the foreground window).
pub fn scan_foreground() -> Result<Vec<Hint>> {
    // SAFETY: all calls are standard UIA/Win32 calls; COM is initialized by the
    // caller on this thread before we get here.
    unsafe {
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        let walker = automation.ControlViewWalker()?;
        let mut hints: Vec<Hint> = Vec::new();

        // 1. The application window — the primary target. Falls back to the last
        //    real app window when the current foreground is the shell/desktop
        //    (e.g. right after clicking a tray icon), so re-triggering keeps
        //    hinting the app the user was actually working in.
        scan_window(&automation, &walker, app_window(), &mut hints);

        // 2. The taskbar's notification area (tray icons, clock, volume, …).
        //    A separate top-level window, so the app-window walk never reaches it.
        //    Mark everything found here as `above`: the taskbar sits at the screen
        //    edge and its icons are small, so labels go above them, not over them.
        let app_count = hints.len();
        if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            scan_window(&automation, &walker, tray, &mut hints);
        }
        for h in hints.iter_mut().skip(app_count) {
            h.above = true;
        }

        Ok(hints)
    }
}

/// Last foreground window that looked like a real application. Used as a
/// fallback when the current foreground is the shell/desktop/taskbar (which
/// happens right after clicking a tray icon), so re-triggering still targets the
/// app the user was working in rather than finding only taskbar elements.
static LAST_APP_HWND: AtomicIsize = AtomicIsize::new(0);

/// Choose the application window to scan: the current foreground if it's a real
/// app window (which we then remember), else the last remembered one.
unsafe fn app_window() -> HWND {
    let fg = GetForegroundWindow();
    if is_app_window(fg) {
        LAST_APP_HWND.store(fg.0 as isize, Ordering::Relaxed);
        fg
    } else {
        HWND(LAST_APP_HWND.load(Ordering::Relaxed) as *mut _)
    }
}

/// Is `hwnd` a normal application window — i.e. not null, and not one of the
/// shell surfaces (taskbar / desktop) or our own overlay? Foregrounds like the
/// taskbar appear after a tray click and would otherwise replace the real app.
unsafe fn is_app_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    let class = String::from_utf16_lossy(&buf[..n as usize]);
    !matches!(
        class.as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW" | "WinHintOverlay"
    )
}

/// Walk one host window's control view into `out`. A null/invalid HWND or a
/// failed UIA lookup is skipped silently so one missing host doesn't abort the
/// whole scan.
unsafe fn scan_window(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    hwnd: HWND,
    out: &mut Vec<Hint>,
) {
    if hwnd.0.is_null() {
        return;
    }
    // Window bounds — used to discard elements whose center lands off-window.
    let mut win_rect = RECT::default();
    if GetWindowRect(hwnd, &mut win_rect).is_err() {
        return;
    }
    let Ok(root) = automation.ElementFromHandle(hwnd) else {
        return;
    };
    walk(walker, &root, &win_rect, 0, out);
}

/// Depth-first walk of the control view, collecting clickable elements.
///
/// Errors on individual elements are swallowed: stale UIA handles are common
/// mid-walk, and one bad node should not abort the whole scan.
unsafe fn walk(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    win_rect: &RECT,
    depth: usize,
    out: &mut Vec<Hint>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_ELEMENTS {
        return;
    }

    consider(element, win_rect, out);

    // GetFirstChildElement / GetNextSiblingElement return Err when there is no
    // such element (null COM pointer), which is how we know to stop iterating.
    let mut child = walker.GetFirstChildElement(element);
    while let Ok(node) = child {
        if out.len() >= MAX_ELEMENTS {
            break;
        }
        walk(walker, &node, win_rect, depth + 1, out);
        child = walker.GetNextSiblingElement(&node);
    }
}

/// Evaluate a single element and push it to `out` if it is a visible,
/// on-window, non-duplicate clickable.
unsafe fn consider(element: &IUIAutomationElement, win_rect: &RECT, out: &mut Vec<Hint>) {
    let Ok(ct) = element.CurrentControlType() else {
        return;
    };
    let Some(label) = clickable_label(ct) else {
        return;
    };

    let Ok(rect) = element.CurrentBoundingRectangle() else {
        return;
    };
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return; // zero / negative area
    }

    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;

    // Skip elements whose center falls outside the host window.
    if cx < win_rect.left || cx > win_rect.right || cy < win_rect.top || cy > win_rect.bottom {
        return;
    }

    // Skip elements the OS reports as off-screen (scrolled out, occluded, etc.).
    if let Ok(off) = element.CurrentIsOffscreen() {
        if off.as_bool() {
            return;
        }
    }

    // Deduplicate elements that resolve to (nearly) the same pixel.
    if out
        .iter()
        .any(|e| (e.cx - cx).abs() < DEDUP_PX && (e.cy - cy).abs() < DEDUP_PX)
    {
        return;
    }

    let name = element
        .CurrentName()
        .map(|b| b.to_string())
        .unwrap_or_default();
    out.push(Hint {
        cx,
        cy,
        top: rect.top,
        name,
        control: label,
        above: false, // overridden for taskbar elements by the caller
    });
}

/// Map a UIA control type to a short label, or `None` if it isn't clickable.
fn clickable_label(ct: UIA_CONTROLTYPE_ID) -> Option<&'static str> {
    let label = match ct {
        UIA_ButtonControlTypeId => "Button",
        UIA_EditControlTypeId => "Edit",
        UIA_HyperlinkControlTypeId => "Hyperlink",
        UIA_CheckBoxControlTypeId => "CheckBox",
        UIA_ComboBoxControlTypeId => "ComboBox",
        UIA_ListItemControlTypeId => "ListItem",
        UIA_MenuItemControlTypeId => "MenuItem",
        UIA_RadioButtonControlTypeId => "RadioButton",
        UIA_TabItemControlTypeId => "TabItem",
        UIA_TreeItemControlTypeId => "TreeItem",
        UIA_DataItemControlTypeId => "DataItem",
        UIA_HeaderItemControlTypeId => "HeaderItem",
        UIA_SplitButtonControlTypeId => "SplitButton",
        _ => return None,
    };
    Some(label)
}
