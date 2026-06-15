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
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTransformPattern,
    IUIAutomationTreeWalker, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
    UIA_ComboBoxControlTypeId, UIA_DataItemControlTypeId, UIA_EditControlTypeId,
    UIA_HeaderItemControlTypeId, UIA_HyperlinkControlTypeId, UIA_ListItemControlTypeId,
    UIA_MenuItemControlTypeId, UIA_PaneControlTypeId, UIA_RadioButtonControlTypeId,
    UIA_SeparatorControlTypeId, UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId,
    UIA_TransformPatternId, UIA_TreeItemControlTypeId, UIA_CONTROLTYPE_ID,
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

/// Maximum tree depth to descend. Web documents (browser content) nest far
/// deeper than native UI — links in a real page sit at depth ~13–24 — so this
/// must be generous enough to reach them. Native apps bottom out well before it.
const MAX_DEPTH: usize = 32;
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

/// DIAGNOSTIC (temporary): dump the full *raw* UIA tree of the foreground window
/// to stdout — every node, unfiltered, with depth, control-type id, and name.
/// Used to investigate why some content (e.g. browser web documents) does or
/// does not appear in the control-view walk. Not used by the daemon.
pub fn dump_tree() -> Result<()> {
    // SAFETY: standard UIA calls; COM is initialized by the caller.
    unsafe {
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        // RawViewWalker shows the complete tree (including non-control nodes),
        // so nothing is hidden by control-view filtering.
        let walker = automation.RawViewWalker()?;
        let hwnd = app_window();
        if hwnd.0.is_null() {
            println!("[dump] no app window to scan");
            return Ok(());
        }
        let root = automation.ElementFromHandle(hwnd)?;
        let mut count = 0usize;
        dump_walk(&walker, &root, 0, &mut count);
        println!("[dump] {count} nodes total");
        Ok(())
    }
}

/// Recursive helper for `dump_tree`. Prints `depth | ct=<id> | name`, indented.
unsafe fn dump_walk(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    depth: usize,
    count: &mut usize,
) {
    if depth > 30 || *count >= 5000 {
        return;
    }
    let ct = element.CurrentControlType().map(|c| c.0).unwrap_or(0);
    let name = element
        .CurrentName()
        .map(|b| b.to_string())
        .unwrap_or_default();
    let name_short: String = name.chars().take(70).collect();
    let indent = depth.min(24);
    println!("{depth:>3} {:indent$}ct={ct} {name_short}", "");
    *count += 1;

    let mut child = walker.GetFirstChildElement(element);
    while let Ok(node) = child {
        if *count >= 5000 {
            break;
        }
        dump_walk(walker, &node, depth + 1, count);
        child = walker.GetNextSiblingElement(&node);
    }
}

/// Smallest pane (each axis, physical px) collected as a splitter-adjacency
/// candidate — filters toolbars/labels that report as panes. Matches
/// `splitter::MIN_PANE_SIZE`.
const PANE_MIN_SIZE: i32 = 80;

/// A divider thinner than this (on its short axis, physical px) and at least
/// `SPLITTER_MIN_LONG` on its long axis is treated as a candidate splitter bar.
const SPLITTER_MAX_THIN: i32 = 10;
/// Minimum long-axis length (physical px) for a thin bar to count as a splitter
/// — filters out tiny separators (e.g. menu dividers) that aren't drag targets.
const SPLITTER_MIN_LONG: i32 = 40;

/// DIAGNOSTIC: dump the foreground window's candidate resize targets in two
/// categories:
///
/// 1. **`[T]` Transform-pattern** elements (`CanResize`/`CanMove`) — the
///    "proper" resizable levels. Proven sparse in practice (Electron apps expose
///    nothing; native apps often only the top-level window).
/// 2. **`[S]` splitter candidates** — UIA `Separator` controls and thin divider
///    bars (short axis ≤ 10px, long axis ≥ 40px). These are the drag targets for
///    *simulated splitter-drag* pane resize:
///    `↔` = vertical bar dragged horizontally; `↕` = horizontal bar dragged
///    vertically. Position is the bar center in physical screen pixels.
///
/// Run `winhint --resizables [n]` (optional countdown to focus a target) to see
/// what an app exposes before wiring it into resize mode.
pub fn dump_resizables() -> Result<()> {
    // SAFETY: standard UIA calls; COM is initialized by the caller.
    unsafe {
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        let walker = automation.RawViewWalker()?;
        let hwnd = app_window();
        if hwnd.0.is_null() {
            println!("[resizables] no app window to scan");
            return Ok(());
        }
        let root = automation.ElementFromHandle(hwnd)?;
        let mut transforms = 0usize;
        let mut splitters = 0usize;
        let mut visited = 0usize;
        resizable_walk(&walker, &root, 0, &mut transforms, &mut splitters, &mut visited);
        println!(
            "[resizables] {transforms} transform-capable + {splitters} splitter candidate(s) \
             across {visited} node(s)"
        );
        Ok(())
    }
}

/// Recursive helper for `dump_resizables`. Prints Transform-capable elements
/// (`[T]`) and splitter candidates (`[S]`), tallying each.
unsafe fn resizable_walk(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    depth: usize,
    transforms: &mut usize,
    splitters: &mut usize,
    visited: &mut usize,
) {
    if depth > 30 || *visited >= 5000 {
        return;
    }
    *visited += 1;

    let ct = element.CurrentControlType().map(|c| c.0).unwrap_or(0);
    let rect = element.CurrentBoundingRectangle().unwrap_or_default();
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let name = element
        .CurrentName()
        .map(|b| b.to_string())
        .unwrap_or_default();
    let name_short: String = name.chars().take(50).collect();
    let indent = depth.min(20);

    if let Some((can_resize, can_move)) = transform_caps(element) {
        if can_resize || can_move {
            *transforms += 1;
            println!(
                "{depth:>3} {:indent$}[T] ct={ct} resize={} move={} {w}x{h}  {name_short}",
                "",
                can_resize as u8,
                can_move as u8,
            );
        }
    }

    // Panes are the containers whose shared edges form the (UIA-invisible)
    // splitters we'll drag geometrically — print their full rect for adjacency
    // analysis. Skip tiny ones (toolbars/labels masquerading as panes).
    if ct == UIA_PaneControlTypeId.0 && w >= PANE_MIN_SIZE && h >= PANE_MIN_SIZE {
        println!(
            "{depth:>3} {:indent$}[P] ct={ct} l={} t={} r={} b={} ({w}x{h})  {name_short}",
            "", rect.left, rect.top, rect.right, rect.bottom,
        );
    }

    if let Some(orient) = splitter_orientation(ct, w, h) {
        *splitters += 1;
        let cx = (rect.left + rect.right) / 2;
        let cy = (rect.top + rect.bottom) / 2;
        println!(
            "{depth:>3} {:indent$}[S] ct={ct} {orient} {w}x{h} @({cx},{cy})  {name_short}",
            "",
        );
    }

    let mut child = walker.GetFirstChildElement(element);
    while let Ok(node) = child {
        if *visited >= 5000 {
            break;
        }
        resizable_walk(walker, &node, depth + 1, transforms, splitters, visited);
        child = walker.GetNextSiblingElement(&node);
    }
}

/// The element's Transform-pattern `(can_resize, can_move)` capabilities, or
/// `None` if it doesn't support the Transform pattern at all.
unsafe fn transform_caps(element: &IUIAutomationElement) -> Option<(bool, bool)> {
    let pattern: IUIAutomationTransformPattern =
        element.GetCurrentPatternAs(UIA_TransformPatternId).ok()?;
    let can_resize = pattern.CurrentCanResize().map(|b| b.as_bool()).unwrap_or(false);
    let can_move = pattern.CurrentCanMove().map(|b| b.as_bool()).unwrap_or(false);
    Some((can_resize, can_move))
}

/// Classify an element as a splitter candidate from its control type and size,
/// returning the drag-orientation arrow, or `None` if it isn't one. A `Separator`
/// control or a thin bar qualifies; orientation follows the long axis (`↕` =
/// wide+short horizontal bar dragged vertically; `↔` = tall+narrow vertical bar
/// dragged horizontally).
fn splitter_orientation(ct: i32, w: i32, h: i32) -> Option<&'static str> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let is_separator = ct == UIA_SeparatorControlTypeId.0;
    let thin_bar = w.min(h) <= SPLITTER_MAX_THIN && w.max(h) >= SPLITTER_MIN_LONG;
    if !is_separator && !thin_bar {
        return None;
    }
    if w >= h {
        Some("↕") // horizontal bar → dragged vertically
    } else {
        Some("↔") // vertical bar → dragged horizontally
    }
}

/// Last foreground window that looked like a real application. Used as a
/// fallback when the current foreground is the shell/desktop/taskbar (which
/// happens right after clicking a tray icon), so re-triggering still targets the
/// app the user was working in rather than finding only taskbar elements.
static LAST_APP_HWND: AtomicIsize = AtomicIsize::new(0);

/// The window a resize would target: the same app window the scan uses, or
/// `None` when there is no real app window to act on (e.g. only the shell is
/// up). `app.rs` uses this to know which window's rect to manipulate.
pub fn target_window() -> Option<HWND> {
    // SAFETY: `app_window` only reads foreground/class info; no state escapes.
    let hwnd = unsafe { app_window() };
    if hwnd.0.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

/// Collect the bounding rects of every sizeable `Pane` under `hwnd`, in physical
/// screen pixels. These are the containers whose shared edges
/// (`splitter::find_boundaries`) form the draggable pane splitters. Tiny panes
/// (below `PANE_MIN_SIZE`) are skipped. A null HWND or UIA failure yields an
/// empty list rather than an error so resize mode degrades gracefully.
pub fn collect_panes(hwnd: HWND) -> Result<Vec<RECT>> {
    if hwnd.0.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: standard UIA calls; COM is initialized by the caller's thread.
    unsafe {
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL)?;
        // RawViewWalker: panes of interest may be hidden from the control view
        // (the diagnostic found Explorer's nav/content panes only via the raw view).
        let walker = automation.RawViewWalker()?;
        let root = automation.ElementFromHandle(hwnd)?;
        let mut out: Vec<RECT> = Vec::new();
        let mut visited = 0usize;
        pane_walk(&walker, &root, 0, &mut out, &mut visited);
        Ok(out)
    }
}

/// Recursive helper for `collect_panes`: push every sizeable pane's rect into `out`.
unsafe fn pane_walk(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    depth: usize,
    out: &mut Vec<RECT>,
    visited: &mut usize,
) {
    if depth > 30 || *visited >= 5000 {
        return;
    }
    *visited += 1;

    if let Ok(ct) = element.CurrentControlType() {
        if ct.0 == UIA_PaneControlTypeId.0 {
            if let Ok(r) = element.CurrentBoundingRectangle() {
                if r.right - r.left >= PANE_MIN_SIZE && r.bottom - r.top >= PANE_MIN_SIZE {
                    out.push(r);
                }
            }
        }
    }

    let mut child = walker.GetFirstChildElement(element);
    while let Ok(node) = child {
        if *visited >= 5000 {
            break;
        }
        pane_walk(walker, &node, depth + 1, out, visited);
        child = walker.GetNextSiblingElement(&node);
    }
}

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
