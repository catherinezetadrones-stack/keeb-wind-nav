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
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics,
    GetWindowRect, LoadCursorW, PostQuitMessage, RegisterClassW, SetWindowPos, ShowWindow,
    TranslateMessage, HWND_TOPMOST, IDC_ARROW, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SWP_SHOWWINDOW, SW_HIDE, WINDOW_EX_STYLE, WM_APP, WM_DESTROY, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::click;
use crate::hints;
use crate::hotkey;
use crate::overlay::{ListRow, RenderItem, ResizeHandleItem, ResizeHud, WebViewOverlay};
use crate::resize::{self, Handle};
use crate::scanner;
use crate::splitter::{self, Boundary};

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
/// Arrow Up/Down → move the list selection, or move a resize handle's
/// horizontal-spanning edge vertically (wparam: 0 = up, 1 = down; lparam bit0 =
/// Shift → fine 1px step in resize mode).
pub const WM_APP_NAV: u32 = WM_APP + 7;
/// Double-tap CapsLock (within the hook's window) → enter resize mode.
pub const WM_APP_RESIZE: u32 = WM_APP + 8;
/// Arrow Left/Right → move a resize handle's vertical-spanning edge horizontally
/// (wparam: 0 = left, 1 = right; lparam bit0 = Shift → fine 1px step). Resize
/// mode only; ignored in hint mode.
pub const WM_APP_NAV_H: u32 = WM_APP + 9;

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
    /// Click target (element center), physical pixels.
    x: i32,
    y: i32,
    /// Element top edge — label anchor when `above` is set.
    top: i32,
    /// Render the label above the element rather than centered on it (tray icons).
    above: bool,
    /// Accessible name, lower-cased once for case-insensitive search.
    name: String,
}

/// What an arrow press will move in resize mode: nothing yet, one of the eight
/// window handles, or a pane splitter (by index into `ResizeState::splitters`).
enum ResizeSelection {
    None,
    Window(Handle),
    Splitter(usize),
}

/// State for an active resize session: which window is being resized, its rect
/// when we started (for Esc restore), its live rect, the pane splitters detected
/// on entry, and what's currently grabbed.
struct ResizeState {
    /// The window being resized.
    target: HWND,
    /// Window rect when resize mode was entered — restored on Esc.
    orig_rect: RECT,
    /// Live rect, updated as the user drags edges with the arrow keys.
    current_rect: RECT,
    /// Pane splitters found on entry; their handles are labeled i, j, k….
    /// `coord` is updated optimistically after each drag (no re-scan).
    splitters: Vec<Boundary>,
    /// The grabbed window handle or splitter, or `None` until a label is typed.
    selection: ResizeSelection,
}

/// The overlay's top-level mode. Hint state (`typed`/`mode`/`selected`/`hints`)
/// lives on `App` and is only meaningful while `Hints`; resize state is
/// self-contained in the `Resize` variant.
enum UiState {
    Idle,
    Hints,
    Resize(ResizeState),
}

/// Shared app state, accessed only from the app thread.
struct App {
    overlay: WebViewOverlay,
    /// Which top-level mode the overlay is in.
    state: UiState,
    /// The single keystroke buffer; interpreted per `mode` (hint mode only).
    typed: String,
    /// Current matching mode (sticky across activations).
    mode: Mode,
    /// Cursor into the ordered visible list (Enter target). Reset to 0 whenever
    /// the result set changes; moved by arrow keys (wrapping top↔bottom).
    selected: usize,
    hints: Vec<HintEntry>,
}

impl App {
    /// Is the overlay showing anything (hint or resize)? Used to guard re-entry.
    fn is_active(&self) -> bool {
        !matches!(self.state, UiState::Idle)
    }
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
                state: UiState::Idle,
                typed: String::new(),
                mode: Mode::Both,
                selected: 0,
                hints: Vec::new(),
            });
        });

        hotkey::set_debug(debug);
        hotkey::install(hwnd)?;
        eprintln!(
            "WinHint running — tap CapsLock to activate · type to search/hint · ↑/↓ select · \
             Enter clicks the selection (Shift+Enter = right-click) · Tab cycles \
             Both/Search/Hints · double-tap CapsLock to resize the window (type a–h to grab a \
             handle · arrows resize, Shift = fine · Enter commits · Esc restores) · \
             CapsLock/Esc to cancel · Ctrl+Alt+Q to quit."
        );
        if debug {
            eprintln!("[debug] key logging on — press some keys; each should print a [hook] line.");
        }

        run_message_loop();

        hotkey::uninstall();
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
        WM_APP_RESIZE => {
            with_app(|app| enter_resize(app, hwnd));
            LRESULT(0)
        }
        WM_APP_KEY => {
            let vk = wparam.0 as u32;
            let shift = (lparam.0 & 1) != 0;
            with_app(|app| {
                if matches!(app.state, UiState::Resize(_)) {
                    handle_resize_key(app, vk);
                } else if matches!(app.state, UiState::Hints) {
                    handle_key(app, hwnd, vk, shift);
                }
            });
            LRESULT(0)
        }
        WM_APP_CONFIRM => {
            let shift = (lparam.0 & 1) != 0;
            with_app(|app| {
                if matches!(app.state, UiState::Resize(_)) {
                    exit_resize(app, hwnd, false); // commit: keep current rect
                } else if matches!(app.state, UiState::Hints) {
                    handle_confirm(app, hwnd, shift);
                }
            });
            LRESULT(0)
        }
        WM_APP_TAB => {
            with_app(|app| {
                if matches!(app.state, UiState::Hints) {
                    handle_tab(app);
                }
            });
            LRESULT(0)
        }
        WM_APP_NAV => {
            let down = wparam.0 == 1;
            let shift = (lparam.0 & 1) != 0;
            with_app(|app| {
                if matches!(app.state, UiState::Resize(_)) {
                    let step = resize_step(shift);
                    handle_resize_nav(app, hwnd, 0, if down { step } else { -step });
                } else if matches!(app.state, UiState::Hints) {
                    handle_nav(app, down);
                }
            });
            LRESULT(0)
        }
        WM_APP_NAV_H => {
            let right = wparam.0 == 1;
            let shift = (lparam.0 & 1) != 0;
            with_app(|app| {
                if matches!(app.state, UiState::Resize(_)) {
                    let step = resize_step(shift);
                    handle_resize_nav(app, hwnd, if right { step } else { -step }, 0);
                }
            });
            LRESULT(0)
        }
        WM_APP_CANCEL => {
            with_app(|app| {
                if matches!(app.state, UiState::Resize(_)) {
                    exit_resize(app, hwnd, true); // restore the original rect
                } else {
                    deactivate(app, hwnd);
                }
            });
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
    if app.is_active() {
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
            top: h.top,
            above: h.above,
            name: h.name.to_lowercase(),
        })
        .collect();
    app.typed.clear(); // mode is sticky — keep whatever was last used
    app.selected = 0;

    render_state(app);
    app.overlay.set_visible(true)?;
    reassert_topmost(hwnd);
    app.state = UiState::Hints;
    hotkey::set_active(true);
    Ok(())
}

/// Re-assert top-of-band z-order and show the overlay without stealing focus.
///
/// Needed not just on first show: interacting with the shell — e.g. clicking a
/// tray icon, which opens a topmost flyout — can leave our overlay buried in the
/// topmost band, and a resize `SetWindowPos` on the target can raise the target
/// over us. `HWND_TOPMOST` + `SWP_SHOWWINDOW` raises us to the front (foreground
/// app keeps focus thanks to `SWP_NOACTIVATE`).
unsafe fn reassert_topmost(hwnd: HWND) {
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
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

    // Only the dedicated Hints mode auto-clicks on a completed hint label. In
    // Both/Search you may be typing a search word whose prefix happens to equal
    // a short hint code, so selection there is Enter-only (or arrows + Enter) to
    // avoid early exits mid-word.
    if app.mode == Mode::Hints {
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
            // Above-anchored labels render off the element's top edge; others
            // render centered on the click point.
            y: if app.hints[i].above {
                app.hints[i].top
            } else {
                app.hints[i].y
            },
            above: app.hints[i].above,
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
    app.state = UiState::Idle;
    app.typed.clear();
    hotkey::set_active(false);
    let _ = app.overlay.set_visible(false);
    let _ = ShowWindow(hwnd, SW_HIDE);
}

/// Step size for one arrow press while resizing: a coarse 8px, or a fine 1px
/// when Shift is held.
fn resize_step(shift: bool) -> i32 {
    if shift {
        1
    } else {
        8
    }
}

/// Most splitter handles we label: `'i'..='z'` is 18 letters (window handles
/// take `'a'..='h'`). Extra splitters past this are dropped.
const MAX_SPLITTERS: usize = 18;

/// The splitter index that label `c` (`'i'..='z'`) grabs, or `None` if `c` is
/// out of range or beyond the `count` of detected splitters.
fn splitter_from_label(c: char, count: usize) -> Option<usize> {
    if !('i'..='z').contains(&c) {
        return None;
    }
    let idx = (c as u8 - b'i') as usize;
    (idx < count).then_some(idx)
}

/// The single-letter label for splitter index `idx` (`0 → 'i'`, `1 → 'j'`, …),
/// the inverse of `splitter_from_label`.
fn splitter_label(idx: usize) -> char {
    (b'i' + idx as u8) as char
}

/// The orientation glyph shown on a splitter handle: a `Vertical` bar is dragged
/// horizontally (`↔`), a `Horizontal` bar vertically (`↕`).
fn orientation_glyph(o: splitter::Orientation) -> &'static str {
    match o {
        splitter::Orientation::Vertical => "↔",
        splitter::Orientation::Horizontal => "↕",
    }
}

/// The handle that label `c` (`'a'..='h'`) grabs, by its index in
/// `Handle::all()` — the same order the labels are assigned in. `None` for any
/// other character.
fn handle_from_label(c: char) -> Option<Handle> {
    if !('a'..='h').contains(&c) {
        return None;
    }
    let i = (c as u8 - b'a') as usize;
    Handle::all().get(i).copied()
}

/// The single-letter label for `handle`, the inverse of `handle_from_label`.
fn label_for_handle(handle: Handle) -> char {
    let i = Handle::all()
        .iter()
        .position(|&h| h == handle)
        .expect("Handle::all() contains every handle");
    (b'a' + i as u8) as char
}

/// Enter resize mode for the tracked app window. Only valid from hint mode; if
/// there is no target window or its rect can't be read, stay in hint mode.
unsafe fn enter_resize(app: &mut App, hwnd: HWND) {
    if !matches!(app.state, UiState::Hints) {
        return;
    }
    let Some(target) = scanner::target_window() else {
        eprintln!("[winhint] resize: no target window — staying in hint mode");
        return;
    };
    let mut rect = RECT::default();
    // SAFETY: target is a live top-level HWND from the scanner; out-param rect.
    if GetWindowRect(target, &mut rect).is_err() {
        eprintln!("[winhint] resize: GetWindowRect failed — staying in hint mode");
        return;
    }

    // Detect pane splitters (shared edges of adjacent panes). Degrades to an
    // empty list — window resize still works — when the app exposes no panes.
    let panes = scanner::collect_panes(target).unwrap_or_default();
    let mut splitters = splitter::find_boundaries(&panes);
    if splitters.len() > MAX_SPLITTERS {
        eprintln!(
            "[winhint] resize: {} splitters found, labeling the first {}",
            splitters.len(),
            MAX_SPLITTERS
        );
        splitters.truncate(MAX_SPLITTERS);
    }

    app.typed.clear();
    app.state = UiState::Resize(ResizeState {
        target,
        orig_rect: rect,
        current_rect: rect,
        splitters,
        selection: ResizeSelection::None,
    });
    hotkey::set_resize_active(true);
    render_resize_state(app);
    // The overlay is already visible (we came from hint mode); keep it on top.
    reassert_topmost(hwnd);
}

/// Handle a forwarded text key in resize mode: a window-handle label (a–h) grabs
/// that handle; a splitter label (i–z, within range) grabs that splitter;
/// anything else is ignored.
fn handle_resize_key(app: &mut App, vk: u32) {
    let Some(c) = text_char(vk) else {
        return;
    };
    let UiState::Resize(rs) = &mut app.state else {
        return;
    };
    if let Some(handle) = handle_from_label(c) {
        rs.selection = ResizeSelection::Window(handle);
    } else if let Some(idx) = splitter_from_label(c, rs.splitters.len()) {
        rs.selection = ResizeSelection::Splitter(idx);
    } else {
        return; // unbound label — leave the current selection untouched
    }
    render_resize_state(app);
}

/// A side effect computed under the `ResizeState` borrow, then performed once the
/// borrow is released: resize the window, or drag a splitter. Splitting it out
/// this way avoids holding a `&mut` to `app.state` across `exit_resize`/render.
enum NavAction {
    /// Nothing grabbed, or an arrow orthogonal to the grabbed splitter.
    None,
    /// Apply `rect` to the target window (the live rect is already updated).
    Window { target: HWND, rect: RECT },
    /// Drag a splitter from `from` to `to` (the boundary is already updated).
    Splitter { from: (i32, i32), to: (i32, i32) },
}

/// Move whatever is grabbed by `(dx, dy)`: a window handle resizes the target
/// live; a splitter is dragged via a simulated mouse drag. No-op until something
/// is grabbed (or for an arrow orthogonal to the grabbed splitter). If the target
/// window has gone away (`SetWindowPos` fails), exit to idle without restoring.
unsafe fn handle_resize_nav(app: &mut App, hwnd: HWND, dx: i32, dy: i32) {
    let action = {
        let UiState::Resize(rs) = &mut app.state else {
            return;
        };
        match rs.selection {
            ResizeSelection::None => NavAction::None,
            ResizeSelection::Window(handle) => {
                let new = resize::apply_handle_move(
                    rs.current_rect,
                    handle,
                    dx,
                    dy,
                    resize::MIN_WIDTH,
                    resize::MIN_HEIGHT,
                );
                rs.current_rect = new;
                NavAction::Window {
                    target: rs.target,
                    rect: new,
                }
            }
            ResizeSelection::Splitter(idx) => {
                let b = rs.splitters[idx];
                // A vertical bar only responds to a horizontal arrow (dx), a
                // horizontal bar only to a vertical arrow (dy); the other axis is
                // a no-op rather than a sideways drag.
                let moves = match b.orientation {
                    splitter::Orientation::Vertical => dx != 0,
                    splitter::Orientation::Horizontal => dy != 0,
                };
                if !moves {
                    NavAction::None
                } else {
                    let from = splitter::drag_point(&b);
                    let to = (from.0 + dx, from.1 + dy);
                    rs.splitters[idx] = splitter::apply_drag(&b, dx, dy);
                    NavAction::Splitter { from, to }
                }
            }
        }
    };

    match action {
        NavAction::None => return,
        NavAction::Window { target, rect } => {
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            // SAFETY: move/size a live top-level window without activating or reordering.
            if SetWindowPos(
                target,
                None,
                rect.left,
                rect.top,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .is_err()
            {
                eprintln!("[winhint] resize: target window gone — exiting");
                exit_resize(app, hwnd, false);
                return;
            }
        }
        NavAction::Splitter { from, to } => {
            // Simulated mouse drag on the shared pane edge. Harmless even if the
            // boundary isn't user-draggable — nothing moves.
            click::drag(from, to);
        }
    }
    // The target move / drag can shuffle z-order; keep our overlay in front.
    reassert_topmost(hwnd);
    render_resize_state(app);
}

/// Push the resize view to the overlay: the eight window-handle pins (yellow),
/// the blue splitter handles, and the size HUD.
fn render_resize_state(app: &App) {
    let UiState::Resize(rs) = &app.state else {
        return;
    };
    let positions = resize::handle_positions(rs.current_rect);
    let mut handles: Vec<ResizeHandleItem> = positions
        .iter()
        .map(|&(handle, x, y)| ResizeHandleItem {
            label: label_for_handle(handle).to_string(),
            x,
            y,
            selected: matches!(rs.selection, ResizeSelection::Window(h) if h == handle),
            splitter: false,
        })
        .collect();

    // Append one blue handle per detected splitter (labels i, j, k…), each
    // carrying an orientation glyph showing which way the arrows drag it.
    for (idx, b) in rs.splitters.iter().enumerate() {
        let (x, y) = splitter::drag_point(b);
        handles.push(ResizeHandleItem {
            label: format!("{}{}", splitter_label(idx), orientation_glyph(b.orientation)),
            x,
            y,
            selected: matches!(rs.selection, ResizeSelection::Splitter(i) if i == idx),
            splitter: true,
        });
    }

    let hud = ResizeHud {
        width: rs.current_rect.right - rs.current_rect.left,
        height: rs.current_rect.bottom - rs.current_rect.top,
        selected_label: selected_hud_label(rs),
    };
    if let Err(e) = app.overlay.render_resize(&handles, &hud) {
        eprintln!("[winhint] resize render failed: {e}");
    }
}

/// The HUD's "grabbed" label for the current selection: the uppercased window
/// handle (`A`–`H`), or the splitter label plus its orientation glyph, or `None`.
fn selected_hud_label(rs: &ResizeState) -> Option<String> {
    match rs.selection {
        ResizeSelection::None => None,
        ResizeSelection::Window(h) => Some(label_for_handle(h).to_uppercase().to_string()),
        ResizeSelection::Splitter(idx) => {
            let glyph = orientation_glyph(rs.splitters[idx].orientation);
            Some(format!(
                "{}{}",
                splitter_label(idx).to_ascii_uppercase(),
                glyph
            ))
        }
    }
}

/// Leave resize mode. `restore` (Esc) puts the window back to its `orig_rect`;
/// otherwise (Enter, or target gone) the current rect is kept.
unsafe fn exit_resize(app: &mut App, hwnd: HWND, restore: bool) {
    if restore {
        if let UiState::Resize(rs) = &app.state {
            let r = rs.orig_rect;
            // SAFETY: restore the original geometry; ignore failure (window gone).
            let _ = SetWindowPos(
                rs.target,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    app.state = UiState::Idle;
    app.typed.clear();
    hotkey::set_resize_active(false);
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
            top: 0,
            above: false,
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

    #[test]
    fn handle_label_maps_to_all_order() {
        // a–h must map to Handle::all() by index (and back).
        for (i, h) in Handle::all().iter().enumerate() {
            let c = (b'a' + i as u8) as char;
            assert_eq!(handle_from_label(c), Some(*h));
            assert_eq!(label_for_handle(*h), c);
        }
    }

    #[test]
    fn handle_label_rejects_out_of_range() {
        assert_eq!(handle_from_label('i'), None); // only 8 handles → a..h
        assert_eq!(handle_from_label('z'), None);
        assert_eq!(handle_from_label('A'), None); // uppercase not accepted
        assert_eq!(handle_from_label('1'), None);
    }

    #[test]
    fn resize_step_fine_with_shift() {
        assert_eq!(resize_step(false), 8); // coarse
        assert_eq!(resize_step(true), 1); // Shift → fine
    }

    #[test]
    fn splitter_label_starts_at_i() {
        assert_eq!(splitter_label(0), 'i');
        assert_eq!(splitter_label(1), 'j');
        assert_eq!(splitter_label(MAX_SPLITTERS - 1), 'z'); // 18 labels → i..=z
    }

    #[test]
    fn splitter_from_label_respects_range_and_count() {
        assert_eq!(splitter_from_label('i', 3), Some(0));
        assert_eq!(splitter_from_label('k', 3), Some(2));
        assert_eq!(splitter_from_label('l', 3), None); // within i..z but beyond count
        assert_eq!(splitter_from_label('h', 3), None); // window-handle range, not a splitter
        assert_eq!(splitter_from_label('z', MAX_SPLITTERS), Some(17));
        assert_eq!(splitter_from_label('I', 3), None); // only lowercase is forwarded
    }

    #[test]
    fn handle_and_splitter_labels_are_disjoint() {
        // a–h never resolve to a splitter; i–z never resolve to a window handle.
        for c in 'a'..='h' {
            assert!(handle_from_label(c).is_some());
            assert_eq!(splitter_from_label(c, MAX_SPLITTERS), None);
        }
        for c in 'i'..='z' {
            assert_eq!(handle_from_label(c), None);
            assert!(splitter_from_label(c, MAX_SPLITTERS).is_some());
        }
    }
}
