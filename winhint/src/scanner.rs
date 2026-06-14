//! UIA tree walk of the foreground window.
//!
//! Collects every element whose control type is in the "clickable" set, with
//! its center point (physical pixels). Filtering rules (depth cap, element cap,
//! off-screen skip, window-rect bounds, pixel dedup) mirror the proven Python
//! prototype (`prototype/winhint.py`).
//!
//! COM must already be initialized on the calling thread (see `main`).

// The windows crate's UIA_*ControlTypeId constants are mixed-case; using them
// as match patterns trips the upper-case-globals lint. They are real consts.
#![allow(non_upper_case_globals)]

use windows::core::Result;
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
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

/// A clickable element discovered during the scan.
pub struct Hint {
    /// Center X in physical screen pixels.
    pub cx: i32,
    /// Center Y in physical screen pixels.
    pub cy: i32,
    /// UIA element name (may be empty).
    pub name: String,
    /// Short control-type label, e.g. "Button".
    pub control: &'static str,
}

/// Maximum tree depth to descend. Matches the prototype's proven value.
const MAX_DEPTH: usize = 12;
/// Cap on collected elements — keeps the scan snappy on dense windows.
const MAX_ELEMENTS: usize = 200;
/// Two centers within this many pixels (each axis) are treated as the same hit.
const DEDUP_PX: i32 = 4;

/// Scan the current foreground window. Returns an empty vec if there is none.
///
/// The foreground HWND is captured at call time, so call this *before* showing
/// any overlay (which would itself become the foreground window).
pub fn scan_foreground() -> Result<Vec<Hint>> {
    // SAFETY: all calls are standard UIA/Win32 calls; COM is initialized by the
    // caller on this thread before we get here.
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Ok(Vec::new());
        }

        // Window bounds — used to discard elements whose center lands off-window.
        let mut win_rect = RECT::default();
        GetWindowRect(hwnd, &mut win_rect)?;

        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        let root = automation.ElementFromHandle(hwnd)?;
        let walker = automation.ControlViewWalker()?;

        let mut hints: Vec<Hint> = Vec::new();
        walk(&walker, &root, &win_rect, 0, &mut hints);
        Ok(hints)
    }
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
        name,
        control: label,
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
