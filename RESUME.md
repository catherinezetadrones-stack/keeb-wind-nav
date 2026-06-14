# RESUME.md — WinHint pickup document

Session date: 2026-06-13. Worked the **App UI / UX Improvements** section of `TODO.md`.

---

## 1. Completed this session

- **CapsLock toggle** (`hotkey.rs`): tap CapsLock to enter hint mode, tap again (or Esc) to leave; Caps key-up suppressed in all orderings so the Caps light never flips.
- **`click.rs`**: refactored shared normalization into `send_at(x,y,down,up)`; added `right_click` alongside `click`.
- **Search-first → 3-mode interaction model** (final, per user). ONE keystroke buffer `typed`, three modes:
  - **Both**: typing matches two ways at once — hint-label prefix (top "hint" section) and name substring (bottom "search" section), shown in a results-list panel under the search bar, split by a horizontal tapered divider. Completing a prefix-free hint label clicks it; Enter clicks the top name match.
  - **Search**: name substring only; Enter clicks the top match; no hint completion.
  - **Hints**: hint code only (letters accepted); completing a label clicks it.
  - **Tab** cycles Both→Search→Hints; **mode is sticky** across activations (only `typed` is cleared on activate/deactivate, never `mode`; default Both).
  - Floating labels stay over elements in all modes (union of the two match sets; hint-prefix matches get an orange-colored prefix). Shift+Enter / Shift+completed-hint = right-click. Empty buffer → Enter is a no-op and nothing is highlighted.
- **Files**: `app.rs` (Mode enum with next/badge/uses_hints/uses_search; `App{typed,mode,hints}`; handle_key/handle_confirm/handle_tab/do_click/enter_target/render_state/row/name_matches/hint_matches/exact_hint; 6 unit tests). `overlay.rs` (`ListRow` struct; `render(floating,query,mode_badge,top,bottom)`; `rows_json` escapes the untrusted name; palette UI = badge + query + results list + `.divider` gradient + floating `.hint` labels; `json_escape_str`). `hotkey.rs` (forwards a–z/0–9/Space/Backspace as `WM_APP_KEY` + Shift bit; Enter→`WM_APP_CONFIRM`; Tab→`WM_APP_TAB`). `CLAUDE.md` Activation row updated.
- **Build green; `cargo test` = 9 passed.** Two reviewer passes: first feature clean (one UX wrinkle fixed); 3-mode redesign clean after fixing one bug (empty-buffer highlighted element 0 — now gated on `has_typed`).

## 2. Current state of in-progress work

Nothing in-progress. All code builds and is unit-tested. The earlier freeze-based Search/HintPick design (`frozen`/`code`/`HintMatch`/`filter_indices`/`top_match`/`visible_indices`) was fully removed and replaced.

## 3. Next steps — ordered and specific

1. **Human interactive test (only unverified part — can't automate real CapsLock + observing a real click).** Run `.\winhint\target\debug\winhint.exe --debug`, focus Notepad/File Explorer, then:
   - Tap CapsLock → palette appears top-center with a "BOTH" badge + floating hint labels on elements.
   - Type part of a name (e.g. "save") → hint-prefix matches list on top, tapered divider, name matches below; floating labels filter to the union; top name match highlighted (teal).
   - Complete a hint label (e.g. "ab") → clicks that element. Type + Enter → clicks top name match. Shift+Enter / Shift+hint → right-click (context menu).
   - Press Tab → badge cycles BOTH→SEARCH→HINTS; verify SEARCH ignores hint completion, HINTS ignores digits/space and only does label picking.
   - Close + reopen → it should start in the last-used mode (sticky).
   - CapsLock/Esc cancels; CapsLock never toggles the Caps light; no keys leak to the focused app while active.
2. Likely tuning knobs if it feels off: palette position/size and `.divider` styling in `overlay.rs shell_html`; whether floating labels should hide in Search mode (currently always shown); substring vs fuzzy/subsequence matching in `name_matches`.
3. Then M5 polish (tray icon, config file, scroll mode, DPI edge cases); broaden element coverage (accept InvokePattern/TogglePattern in `scanner.rs`).

## 4. Decisions made this session

- **Three sticky modes** (Both/Search/Hints) cycled by Tab, replacing the freeze-based two-mode design. "Both" shows both match interpretations at once (hint-prefix top / name-search bottom) — this is how the search-vs-hint ambiguity is resolved: no disambiguation needed, both are shown.
- **Floating labels kept** in all modes (user: "you don't have to change your design much") — the results list is an *addition* under the search bar, not a replacement. Divider is **horizontal**, tapered (CSS gradient, fades at both ends), only drawn in Both when both sections are non-empty.
- **Sticky mode**: persists across activations because `App` lives for the whole process and `activate`/`deactivate` clear only `typed`.
- **Empty buffer**: no top match, no highlight, Enter no-op (reviewer-caught consistency fix).
- Hook stays minimal (only `GetAsyncKeyState` for Shift + `PostMessage`); all logic in the wndproc on the STA thread.

## 5. Discoveries

- **Build lock (not a bug):** `cargo build` fails `Access is denied (os error 5)` if a `winhint.exe` daemon is still running. Fix: `Get-Process winhint | Stop-Process -Force` first.
- **`scanner.rs` already captures `name`** — lower-cased once at `activate`.
- JS `slice` vs Rust `chars().count()` for the typed prefix are equal only because labels are ASCII `[a-z]` (latent fragility if labels go non-ASCII; out of scope).
- `planner`/`reviewer` subagents ARE registered now; used `Plan` for the first design and `reviewer` after each implementation.

## 6. Verification steps (run first next session)

```powershell
Get-Process winhint -ErrorAction SilentlyContinue | Stop-Process -Force
cargo build --manifest-path winhint/Cargo.toml
cargo test  --manifest-path winhint/Cargo.toml   # expect 9 passed
.\winhint\target\debug\winhint.exe --debug       # then the interactive test in step 3.1
```
Expected: clean build, 9 tests pass, and the 3-mode flow above works.
