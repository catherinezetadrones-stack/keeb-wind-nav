//! Application runtime: owns the overlay window, the shared state, the window
//! procedure, and the message loop. The keyboard hook (see `hotkey.rs`) posts
//! `WM_APP_*` intents here; all scan/filter/click work happens in the window
//! proc, off the hook.
//!
//! Everything runs on one STA thread, so shared state lives in a thread-local
//! `RefCell` rather than behind a mutex. The hook only ever `PostMessage`s
//! (async), so it never re-enters a borrow held by the window proc.

use std::cell::RefCell;

use anyhow::Result;
use windows::core::w;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics, LoadCursorW,
    PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage, IDC_ARROW, MSG,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE,
    SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WM_APP, WM_DESTROY, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::click;
use crate::hints;
use crate::hotkey;
use crate::overlay::{ListRow, RenderItem, WebViewOverlay};
use crate::scanner;

/// Hotkey pressed → enter hint mode.
pub const WM_APP_SHOW: u32 = WM_APP + 1;
/// A text key was typed (wparam = virtual-key code; lparam bit0 = Shift held,
/// ignored for text input but kept for protocol uniformity).
pub const WM_APP_KEY: u32 = WM_APP + 2;
/// Escape / CapsLock pressed → leave hint mode.
pub const WM_APP_CANCEL: u32 = WM_APP + 3;
/// Quit chord pressed → exit the app.
pub const WM_APP_QUIT: u32 = WM_APP + 4;
/// Enter pressed → act on the selected match (lparam bit0 = Shift → right-click).
pub const WM_APP_CONFIRM: u32 = WM_APP + 5;
/// Tab pressed → cycle the matching mode.
pub const WM_APP_TAB: u32 = WM_APP + 6;
/// Arrow Up/Down → move the list selection (wparam: 0 = up, 1 = down).
pub const WM_APP_NAV: u32 = WM_APP + 7;

const WINDOW_CLASS: windows::core::PCWSTR = w!("WinHintOverlay");

/// Backspace virtual-key code.
const VK_BACK: u32 = 0x08;

/// Which matching mode hint mode is in. Typing feeds a single `typed` buffer;
/// the mode decides how it's interpreted. Tab cycles Both → Search → Hints and
/// the last-used mode is sticky across activations.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Match both ways at once: elements whose hint code starts with `typed`
    /// (top section) and elements whose name contains `typed` (below the divider).
    Both,
    /// `typed` filters elements by accessible name only.
    Search,
    /// `typed` is a hint code only (the classic label-pick).
    Hints,
}

impl Mode {
    /// The next mode in the Tab cycle.
    fn next(self) -> Mode {
        match self {
            Mode::Both => Mode::Search,
            Mode::Search => Mode::Hints,
            Mode::Hints => Mode::Both,
        }
    }

    /// Short badge text for the overlay.
    fn badge(self) -> &'static str {
        match self {
            Mode::Both => "BOTH",
            Mode::Search => "SEARCH",
            Mode::Hints => "HINTS",
        }
    }

    /// Does this mode interpret `typed` as a hint code (so a completed label clicks)?
    fn uses_hints(self) -> bool {
        matches!(self, Mode::Both | Mode::Hints)
    }

    /// Does this mode search element names?
    fn uses_search(self) -> bool {
        matches!(self, Mode::Both | Mode::Search)
    }
}

/// One scanned, labeled hint target.
struct HintEntry {
    label: String,
    x: i32,
    y: i32,
    /// Accessible name, lower-cased once for case-insensitive search.
    name: String,
}

/// Shared app state, accessed only from the app thread.
struct App {
    overlay: WebViewOverlay,
    active: bool,
    /// The single keystroke buffer; interpreted per `mode`.
    typed: String,
    /// Current matching mode (sticky across activations).
    mode: Mode,
    /// Cursor into the ordered visible list (Enter target). Reset to 0 whenever
    /// the result set changes; moved by arrow keys (wrapping top↔bottom).
    selected: usize,
    hints: Vec<HintEntry>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Build the window + overlay, install the hook, and run the message loop.
pub fn run(debug: bool) -> Result<()> {
    // SAFETY: window/COM setup; COM (STA) is initialized by the caller.
    unsafe {
        let hinstance: HMODULE = GetModuleHandleW(None)?;
        register_class(hinstance);

        let (vx, vy, vw, vh) = virtual_screen();
        let hwnd = create_window(hinstance, vx, vy, vw, vh)?;

        // Overlay starts hidden; the window is only shown during hint mode.
        let overlay = WebViewOverlay::new(hwnd, vw, vh, debug)?;

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                overlay,
                active: false,
                typed: String::new(),
                mode: Mode::Both,
                selected: 0,
                hints: Vec::new(),
            });
        });

        hotkey::set_debug(debug);
        let hook = hotkey::install(hwnd)?;
        eprintln!(
            "WinHint running — tap CapsLock to activate · type to search/hint · ↑/↓ select · \
             Enter clicks the selection (Shift+Enter = right-click) · Tab cycles \
             Both/Search/Hints · CapsLock/Esc to cancel · Ctrl+Alt+Q to quit."
        );
        if debug {
            eprintln!("[debug] key logging on — press some keys; each should print a [hook] line.");
        }

        run_message_loop();

        hotkey::uninstall(hook);
    }
    Ok(())
}

/// (x, y, width, height) of the whole virtual desktop in physical pixels.
unsafe fn virtual_screen() -> (i32, i32, i32, i32) {
    (
        GetSystemMetrics(SM_XVIRTUALSCREEN),
        GetSystemMetrics(SM_YVIRTUALSCREEN),
        GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_CYVIRTUALSCREEN),
    )
}

unsafe fn register_class(hinstance: HMODULE) {
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance.into(),
        lpszClassName: WINDOW_CLASS,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc);
}

unsafe fn create_window(hinstance: HMODULE, x: i32, y: i32, w: i32, h: i32) -> Result<HWND> {
    let ex_style: WINDOW_EX_STYLE = WS_EX_NOREDIRECTIONBITMAP // DComp-only surface
        | WS_EX_TOPMOST
        | WS_EX_NOACTIVATE // never steal focus from the target window
        | WS_EX_TRANSPARENT // mouse clicks pass through
        | WS_EX_TOOLWINDOW; // hide from Alt-Tab
    let hwnd = CreateWindowExW(
        ex_style,
        WINDOW_CLASS,
        w!("WinHint"),
        WS_POPUP,
        x,
        y,
        w,
        h,
        None,
        None,
        Some(hinstance.into()),
        None,
    )?;
    Ok(hwnd)
}

unsafe fn run_message_loop() {
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_SHOW => {
            with_app(|app| {
                if let Err(e) = activate(app, hwnd) {
                    eprintln!("[winhint] activate failed: {e}");
                }
            });
            LRESULT(0)
        }
        WM_APP_KEY => {
            let vk = wparam.0 as u32;
            let shift = (lparam.0 & 1) != 0;
            with_app(|app| handle_key(app, hwnd, vk, shift));
            LRESULT(0)
        }
        WM_APP_CONFIRM => {
            let shift = (lparam.0 & 1) != 0;
            with_app(|app| handle_confirm(app, hwnd, shift));
            LRESULT(0)
        }
        WM_APP_TAB => {
            with_app(|app| handle_tab(app));
            LRESULT(0)
        }
        WM_APP_NAV => {
            let down = wparam.0 == 1;
            with_app(|app| handle_nav(app, down));
            LRESULT(0)
        }
        WM_APP_CANCEL => {
            with_app(|app| deactivate(app, hwnd));
            LRESULT(0)
        }
        WM_APP_QUIT | WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Run `f` with the live `App`, if initialized.
fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            f(app);
        }
    });
}

/// Enter hint mode: scan the foreground window, show labeled hints.
unsafe fn activate(app: &mut App, hwnd: HWND) -> Result<()> {
    if app.active {
        return Ok(());
    }

    eprintln!("[winhint] hotkey fired — scanning foreground window...");
    let scanned = scanner::scan_foreground()?;
    eprintln!("[winhint] found {} clickable element(s)", scanned.len());
    if scanned.is_empty() {
        eprintln!("[winhint] (nothing to hint — is a real app focused, not this terminal?)");
        return Ok(()); // nothing to hint; stay idle
    }

    let labels = hints::labels(scanned.len());
    app.hints = scanned
        .into_iter()
        .zip(labels)
        .map(|(h, label)| HintEntry {
            label,
            x: h.cx,
            y: h.cy,
            name: h.name.to_lowercase(),
        })
        .collect();
    app.typed.clear(); // mode is sticky — keep whatever was last used
    app.selected = 0;

    render_state(app);
    app.overlay.set_visible(true)?;
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    app.active = true;
    hotkey::set_active(true);
    Ok(())
}

/// Map a forwarded virtual-key code to the character it contributes to a text
/// buffer (lower-cased letters, digits, space), if any.
fn text_char(vk: u32) -> Option<char> {
    match vk {
        0x41..=0x5A => Some((b'a' + (vk - 0x41) as u8) as char), // a-z
        0x30..=0x39 => Some((b'0' + (vk - 0x30) as u8) as char), // 0-9
        0x20 => Some(' '),                                       // VK_SPACE
        _ => None,
    }
}

/// Handle a forwarded text key into the single `typed` buffer. A completed hint
/// label clicks immediately (in modes that interpret hints).
unsafe fn handle_key(app: &mut App, hwnd: HWND, vk: u32, shift: bool) {
    if vk == VK_BACK {
        app.typed.pop();
        app.selected = 0; // result set changed
        render_state(app);
        return;
    }
    let Some(ch) = text_char(vk) else {
        return;
    };
    // In Hints mode only letters form a hint code; digits/space are ignored.
    if app.mode == Mode::Hints && !ch.is_ascii_lowercase() {
        return;
    }
    app.typed.push(ch);

    // A completed, prefix-free hint label is unambiguous → click it.
    if app.mode.uses_hints() {
        if let Some(idx) = exact_hint(&app.hints, &app.typed) {
            do_click(app, hwnd, idx, shift);
            return;
        }
    }
    app.selected = 0; // result set changed
    render_state(app);
}

/// Handle an arrow-key move (Up/Down) over the visible list, wrapping top↔bottom.
/// No-op when the list is hidden (nothing typed) or empty.
fn handle_nav(app: &mut App, down: bool) {
    if app.typed.is_empty() {
        return;
    }
    let n = ordered_visible(app).len();
    app.selected = wrap_index(app.selected, n, down);
    render_state(app);
}

/// Handle Enter: click the top match (Shift → right-click). Empty buffer is a
/// no-op so a bare Enter never clicks an arbitrary first element.
unsafe fn handle_confirm(app: &mut App, hwnd: HWND, shift: bool) {
    if app.typed.is_empty() {
        return;
    }
    if let Some(idx) = enter_target(app) {
        do_click(app, hwnd, idx, shift);
    }
}

/// Handle Tab: cycle Both → Search → Hints. The keystroke buffer is kept so the
/// new mode simply re-interprets what's already typed.
fn handle_tab(app: &mut App) {
    app.mode = app.mode.next();
    app.selected = 0; // the result set / ordering changes with the mode
    render_state(app);
}

/// Click the element at `hints[idx]`, leaving hint mode first so the overlay is
/// hidden before the synthetic click lands.
unsafe fn do_click(app: &mut App, hwnd: HWND, idx: usize, shift: bool) {
    let (x, y) = (app.hints[idx].x, app.hints[idx].y);
    deactivate(app, hwnd);
    if shift {
        click::right_click(x, y);
    } else {
        click::click(x, y);
    }
}

/// The ordered visible list: hint-prefix matches first (the top section), then
/// name-search matches not already shown (the bottom section). This single
/// ordering drives the floating labels, the results list, and arrow-key
/// selection so they can never disagree. An empty buffer matches everything.
fn ordered_visible(app: &App) -> Vec<usize> {
    let hint_idx = if app.mode.uses_hints() {
        hint_matches(&app.hints, &app.typed)
    } else {
        Vec::new()
    };
    let name_idx = if app.mode.uses_search() {
        name_matches(&app.hints, &app.typed)
    } else {
        Vec::new()
    };
    let in_hint: std::collections::HashSet<usize> = hint_idx.iter().copied().collect();
    let mut ordered = hint_idx;
    ordered.extend(name_idx.into_iter().filter(|i| !in_hint.contains(i)));
    ordered
}

/// The element Enter acts on: the currently selected row. `None` if nothing matches.
fn enter_target(app: &App) -> Option<usize> {
    let ordered = ordered_visible(app);
    if ordered.is_empty() {
        return None;
    }
    ordered
        .get(app.selected.min(ordered.len() - 1))
        .copied()
}

/// Move `sel` by one within `[0, n)`, wrapping around both ends.
fn wrap_index(sel: usize, n: usize, down: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if down {
        (sel + 1) % n
    } else {
        (sel + n - 1) % n
    }
}

/// Push the current UI state to the overlay: floating labels over elements, plus
/// the results list (hint section on top, name-search section below the divider).
fn render_state(app: &App) {
    let typed_len = app.typed.chars().count();
    let has_typed = typed_len > 0;

    let ordered = ordered_visible(app);
    // Which visible elements are hint-prefix matches (colored prefix + top section).
    let hint_set: std::collections::HashSet<usize> = if app.mode.uses_hints() {
        hint_matches(&app.hints, &app.typed).into_iter().collect()
    } else {
        std::collections::HashSet::new()
    };
    // Only highlight a target once something is typed (empty buffer → no meaningful
    // selection, and Enter is a no-op). `selected` is clamped to the list length.
    let sel = if has_typed && !ordered.is_empty() {
        ordered.get(app.selected.min(ordered.len() - 1)).copied()
    } else {
        None
    };

    // Floating labels follow the ordered set (already deduped). Hint-prefix
    // matches get their prefix colored; name-only matches show the bare label.
    let floating: Vec<RenderItem> = ordered
        .iter()
        .map(|&i| RenderItem {
            label: app.hints[i].label.clone(),
            x: app.hints[i].x,
            y: app.hints[i].y,
            typed: if hint_set.contains(&i) { typed_len } else { 0 },
            selected: sel == Some(i),
        })
        .collect();

    // Results list rows (only once something is typed). Top = hint matches;
    // bottom = name-only matches. Both drawn from the same ordered set.
    let (top, bottom) = if has_typed {
        let top: Vec<ListRow> = ordered
            .iter()
            .copied()
            .filter(|i| hint_set.contains(i))
            .map(|i| row(app, i, sel))
            .collect();
        let bottom: Vec<ListRow> = ordered
            .iter()
            .copied()
            .filter(|i| !hint_set.contains(i))
            .map(|i| row(app, i, sel))
            .collect();
        (top, bottom)
    } else {
        (Vec::new(), Vec::new())
    };

    if let Err(e) =
        app.overlay
            .render(&floating, &app.typed, app.mode.badge(), &top, &bottom)
    {
        eprintln!("[winhint] render failed: {e}");
    }
}

/// Build a results-list row for element `i`, marking it selected if it's the
/// Enter target.
fn row(app: &App, i: usize, sel: Option<usize>) -> ListRow {
    ListRow {
        label: app.hints[i].label.clone(),
        name: app.hints[i].name.clone(),
        selected: sel == Some(i),
    }
}

/// Element indices whose accessible name contains `typed` (case-insensitive).
/// An empty buffer matches everything. Names are stored pre-lowercased.
fn name_matches(hints: &[HintEntry], typed: &str) -> Vec<usize> {
    if typed.is_empty() {
        return (0..hints.len()).collect();
    }
    let q = typed.to_lowercase();
    hints
        .iter()
        .enumerate()
        .filter(|(_, h)| h.name.contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// Element indices whose hint label starts with `typed`. An empty buffer matches
/// everything; a buffer containing non-label chars (digits/space) matches none.
fn hint_matches(hints: &[HintEntry], typed: &str) -> Vec<usize> {
    hints
        .iter()
        .enumerate()
        .filter(|(_, h)| h.label.starts_with(typed))
        .map(|(i, _)| i)
        .collect()
}

/// The element whose hint label exactly equals `typed`, if any (prefix-free, so
/// at most one).
fn exact_hint(hints: &[HintEntry], typed: &str) -> Option<usize> {
    hints.iter().position(|h| h.label == typed)
}

/// Leave hint mode and hide the overlay. `mode` is intentionally preserved
/// (sticky across activations); only the keystroke buffer is cleared.
unsafe fn deactivate(app: &mut App, hwnd: HWND) {
    app.active = false;
    app.typed.clear();
    hotkey::set_active(false);
    let _ = app.overlay.set_visible(false);
    let _ = ShowWindow(hwnd, SW_HIDE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, name: &str) -> HintEntry {
        HintEntry {
            label: label.to_string(),
            x: 0,
            y: 0,
            name: name.to_lowercase(),
        }
    }

    #[test]
    fn name_empty_matches_all() {
        let hints = vec![entry("a", "Save"), entry("b", "Cancel")];
        assert_eq!(name_matches(&hints, ""), vec![0, 1]);
    }

    #[test]
    fn name_is_case_insensitive_substring() {
        let hints = vec![entry("a", "Save File"), entry("b", "Cancel"), entry("c", "Saved")];
        assert_eq!(name_matches(&hints, "SAV"), vec![0, 2]);
        assert_eq!(name_matches(&hints, "file"), vec![0]);
    }

    #[test]
    fn name_no_match_is_empty() {
        let hints = vec![entry("a", "Save"), entry("b", "Cancel")];
        assert!(name_matches(&hints, "xyz").is_empty());
    }

    #[test]
    fn hint_matches_by_label_prefix() {
        let hints = vec![entry("aa", "Save"), entry("ab", "Open"), entry("ba", "Quit")];
        assert_eq!(hint_matches(&hints, ""), vec![0, 1, 2]); // empty → all
        assert_eq!(hint_matches(&hints, "a"), vec![0, 1]);
        assert_eq!(hint_matches(&hints, "ba"), vec![2]);
        assert!(hint_matches(&hints, "5").is_empty()); // non-label char → none
    }

    #[test]
    fn exact_hint_is_unique() {
        let hints = vec![entry("aa", "Save"), entry("ab", "Open")];
        assert_eq!(exact_hint(&hints, "aa"), Some(0));
        assert_eq!(exact_hint(&hints, "ab"), Some(1));
        assert_eq!(exact_hint(&hints, "a"), None); // prefix, not exact
        assert_eq!(exact_hint(&hints, "zz"), None);
    }

    #[test]
    fn mode_cycle_order() {
        assert!(matches!(Mode::Both.next(), Mode::Search));
        assert!(matches!(Mode::Search.next(), Mode::Hints));
        assert!(matches!(Mode::Hints.next(), Mode::Both));
    }

    #[test]
    fn wrap_index_wraps_both_ends() {
        assert_eq!(wrap_index(0, 3, true), 1);
        assert_eq!(wrap_index(2, 3, true), 0); // bottom → top
        assert_eq!(wrap_index(0, 3, false), 2); // top → bottom
        assert_eq!(wrap_index(1, 3, false), 0);
        assert_eq!(wrap_index(0, 0, true), 0); // empty list is safe
        assert_eq!(wrap_index(0, 1, true), 0); // single item stays
    }
}
