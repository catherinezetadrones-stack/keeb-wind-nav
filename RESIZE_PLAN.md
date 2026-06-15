# Pane-Boundary (Splitter) Resize — Plan (foundation built, integration pending)

Extends the window-resize feature with **pane-splitter resize**: resize the
boundary between two panes (e.g. Explorer's nav | content panes) by simulating a
mouse drag on their shared edge. Splitters are NOT UIA elements, but the panes
are — two adjacent panes' shared edge *is* the splitter, located geometrically.

## Approved design decisions

- **Fold into the existing resize mode** (no new gesture). Double-tap CapsLock
  still enters resize. Window handles stay `a`–`h`; splitter handles continue
  `i`, `j`, `k`… Type a label to grab either kind.
- **Locate splitters geometrically** from pane adjacency (done — `splitter.rs`).
- **Drag model: one complete press→move→release per arrow press** (`click::drag`,
  done). 8px step / Shift = 1px, same as window resize.
- **Update-by-delta, no re-scan** after each drag (optimistic; may drift if the
  app clamps the drag — refreshed on next resize-mode entry).
- **Esc/CapsLock "restore" reverts only the window rect, NOT splitter drags**
  (there's no clean undo for a drag). Documented limitation.

## Empirical grounding (File Explorer, physical px)

- UIA Transform pattern is a DEAD END for inner resize (VS Code exposes nothing;
  Explorer panes report `resize=0`). Splitters are NOT UIA elements either.
- Panes ARE visible: nav `l=94 t=290 r=373 b=913`, content `l=377 t=290 r=1122
  b=913` → shared vertical edge at x≈375 spanning y 290..913 = the draggable
  splitter. `splitter::find_boundaries` produces exactly this (unit-tested).
- Diagnostic to re-check any app: `winhint --resizables [n]` (n = countdown secs).
  Prints `[T]` transform-capable, `[S]` splitter candidates, `[P]` pane rects.

## DONE this session (foundation — all compiles clean, 30 tests pass)

- **`winhint/src/splitter.rs` (NEW)** — pure geometry, no Win32. `enum
  Orientation {Vertical, Horizontal}` (named for the bar; Vertical bar = dragged
  horizontally), `struct Boundary {orientation, coord, span_start, span_end}`,
  `find_boundaries(&[RECT]) -> Vec<Boundary>` (filter < MIN_PANE_SIZE → pairwise
  adjacency within EDGE_TOLERANCE w/ ≥ MIN_OVERLAP cross-overlap → dedup/union →
  sorted), `drag_point(&Boundary) -> (i32,i32)` (span midpoint on the line),
  `apply_drag(&Boundary, dx, dy) -> Boundary` (orientation picks the axis).
  Consts: MIN_PANE_SIZE=80, EDGE_TOLERANCE=8, MIN_OVERLAP=40, DEDUP_COORD=8.
  **10 unit tests** incl. the real Explorer case. `mod splitter;` added to main.rs.
- **`winhint/src/scanner.rs`** — `pub fn collect_panes(hwnd) -> Result<Vec<RECT>>`
  + `pane_walk` helper (RawViewWalker, collects `UIA_PaneControlTypeId` rects ≥
  `PANE_MIN_SIZE`). New `const PANE_MIN_SIZE: i32 = 80` (also now used by the
  `dump_resizables` diagnostic). Returns empty (not error) on null HWND.
- **`winhint/src/click.rs`** — `pub fn drag(from, to)` (press → interpolated
  intermediate moves → release, all in one SendInput array; nothing held across
  calls). Refactored shared `input_at` + `normalize` helpers out of `send_at`.

These three are independent and currently unused → expected `dead_code` warnings
on `collect_panes`, `pane_walk`, `drag` until app.rs wires them (step 2 below).

## NEXT — integration (Tasks 4, 5, 6 of the planner's plan), in order

### 1. `overlay.rs` — distinguish splitter handles (Task 4, do first; small)
- Add a field to `ResizeHandleItem` to mark splitter handles, e.g. `pub
  splitter: bool` (simplest). Serialize it in `resize_handles_json` (e.g. add
  `,p:{0/1}`). In the `renderResize` JS handle loop, add class `splitter` when
  set; add CSS `.hint.handle.splitter{border-color:#7aa2ff;color:#7aa2ff;}` (a
  blue, distinct from the yellow window handles). Optionally append a `↔`/`↕`
  glyph to the label text for orientation. Add a legend chip e.g.
  `chip(['i','…'],'splitter')`.

### 2. `app.rs` — state + wiring (Task 5; the big one)
- Add `use crate::splitter::{self, Boundary};` and `use crate::click;` (already
  imported) and `scanner::collect_panes`.
- **`ResizeState`**: add `splitters: Vec<Boundary>`. Replace `selected:
  Option<Handle>` with:
  ```rust
  enum ResizeSelection { None, Window(Handle), Splitter(usize) }
  ```
  Update every read of `rs.selected` (in `render_resize_state`, `handle_resize_key`,
  `handle_resize_nav`).
- **`enter_resize`**: after computing `current_rect`, do
  `let panes = scanner::collect_panes(target).unwrap_or_default();` then
  `splitters = splitter::find_boundaries(&panes)` (cap to 17 → labels i..z; if
  more, truncate + debug log). Store in ResizeState. Empty is fine (no splitter
  handles; window resize still works).
- **Labels**: window handles a–h via existing `label_for_handle`/`handle_from_label`.
  Splitter idx → label `(b'i' + idx) as char`; add `splitter_from_label(c) ->
  Option<usize>` for `'i'..='z'` within `splitters.len()`.
- **`handle_resize_key`**: if char maps to a window handle → `Window(h)`; else if
  it maps to a splitter idx → `Splitter(idx)`; else ignore. Then re-render.
- **`handle_resize_nav(app, hwnd, dx, dy)`**: branch on selection:
  - `Window(h)`: existing `apply_handle_move` + `SetWindowPos` path (unchanged).
  - `Splitter(idx)`: gate on orientation — Vertical boundary only responds to
    dx (Left/Right = WM_APP_NAV_H), Horizontal only to dy (Up/Down = WM_APP_NAV);
    orthogonal arrow = no-op. Compute `from = splitter::drag_point(&b)`, `to =
    (from.0+dx, from.1+dy)`, call `click::drag(from, to)`, then `b =
    splitter::apply_drag(&b, dx, dy)` (write back into `rs.splitters[idx]`),
    `reassert_topmost(hwnd)`, re-render. NOTE: before dragging, optionally verify
    `scanner::target_window() == Some(rs.target)`; if the target's gone, treat
    like the existing window "target gone" path (exit_resize false).
  - Both NAV and NAV_H already call into the nav handler in wndproc — currently
    NAV passes (0, ±step) and NAV_H passes (±step, 0). That already works for
    splitters; just make sure NAV_H reaches the splitter branch (it currently
    early-returns unless Resize — fine).
- **`render_resize_state`**: after building the 8 window-handle items, append one
  `ResizeHandleItem` per splitter (label `i+idx`, pos = `drag_point`, `selected =
  matches Splitter(idx)`, `splitter: true`). HUD can show the selected splitter's
  orientation.
- **Commit/cancel**: Enter (`exit_resize(false)`) already just exits — fine
  (drags happened live). Esc/CapsLock (`exit_resize(true)`) restores the window
  rect only; splitter drags are not reverted (documented). No code change needed
  beyond a possible eprintln note.

### 3. `hotkey.rs` — verify only (Task 6; likely no changes)
- Letters i–z already forwarded as WM_APP_KEY while resize-active; Left/Right
  already forwarded as WM_APP_NAV_H, Up/Down as WM_APP_NAV, both with Shift bit.
  So no new constants/routing. Just confirm.

## After wiring
- Stop any running `winhint.exe` (locks the binary), `cargo build`, `cargo test`
  (expect 30 + new app tests passing). Launch, open File Explorer, double-tap
  CapsLock → expect blue `i` handle on the nav/content splitter → type `i` →
  Left/Right arrows drag it (Shift = fine). Then run the `reviewer` subagent per
  CLAUDE.md and commit only after the user validates.

## Gotchas
- `cargo test` does NOT rebuild `winhint.exe`; a running daemon locks it
  (`Get-Process winhint | Stop-Process -Force` first).
- Single-instance guard is live — stop the old daemon before launching a new one.
- Some pane boundaries aren't actually user-draggable (e.g. ribbon/content edge);
  dragging them is harmless (nothing moves) but the handle still appears. Accepted.
- VS Code (Electron) exposes no panes → no splitter handles there. Expected.
