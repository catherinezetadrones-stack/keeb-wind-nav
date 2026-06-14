# CLAUDE.md — Keeb-Wind-Nav

Keyboard-driven hint-mode navigation for Windows. Press a hotkey, get alphabetic
labels on every clickable UI element, type to click. Like Vimac / Homerow on macOS
but for Windows.

---

## Project overview

| Thing | Detail |
|---|---|
| Target OS | Windows 10 / 11 (x64) |
| Primary language | Rust |
| Overlay renderer | WebView2 (via `webview2-com`) — HTML/CSS hint labels |
| UIA scanning | `windows-rs` — `Win32_UI_Accessibility` feature |
| Hotkey hook | `windows-rs` — `SetWindowsHookEx(WH_KEYBOARD_LL)` |
| Click dispatch | `windows-rs` — `SendInput` |
| Activation | Tap CapsLock to enter hint mode; tap CapsLock or Esc to leave; Ctrl+Alt+Q quits |

A working **Python prototype** (`winhint.py`) exists in `/prototype/`. It proved out
the full pipeline (UIA scan → transparent overlay → hotkey listener → simulated click)
and is the canonical reference for expected behaviour. Do not delete it; reference it
when the Rust behaviour is ambiguous.

---

## Repository layout

```
winhint/
├── CLAUDE.md               ← you are here
├── KICKOFF_PROMPT.md       ← initial context used to start this project
├── Cargo.toml
├── Cargo.lock
├── prototype/
│   └── winhint.py          ← Python reference implementation
├── src/
│   ├── main.rs             ← entry point; message loop
│   ├── hotkey.rs           ← global WH_KEYBOARD_LL hook
│   ├── scanner.rs          ← IUIAutomation tree walk
│   ├── overlay.rs          ← Win32 layered window + WebView2 host
│   ├── hints.rs            ← hint string generation and filtering
│   ├── click.rs            ← SendInput click dispatch
│   └── config.rs           ← hotkey, colours, limits (user-editable)
└── overlay-ui/             ← HTML/CSS/JS for the hint labels (loaded by WebView2)
    ├── index.html
    ├── style.css
    └── hints.js
```

Layout is aspirational at first; create files as milestones demand them.

---

## Build & run

```bash
# Debug build + run (prints scan results to console)
cargo run

# Release build
cargo build --release
# Output: target/release/winhint.exe

# Run release build
./target/release/winhint.exe
```

There is no test suite yet. Manual testing procedure: run the binary, press
`Ctrl+Alt+Space` over Notepad or File Explorer, verify hints appear on buttons
and inputs.

---

## Key Windows concepts to keep in mind

### UI Automation (UIA)
- COM-based accessibility API; the right tool for enumerating native Win32, WinForms,
  WPF, and some Qt windows
- Entry point: `IUIAutomation` (CLSID `{ff48dba4-60ef-4201-aa87-54103eef594e}`)
- Walk the tree with `IUIAutomationTreeWalker`; filter by `UIA_ControlTypePropertyId`
- `BoundingRectangle` returns screen coordinates (physical pixels, not DIPs)
- **Does not see into Electron or WebView2 app content** — buttons inside a web page
  are invisible. This is a known limitation; do not attempt to fix it until M5+.
- `IsOffscreen` and zero-size rects must be filtered out; the tree is full of them

### Layered window (overlay)
- `CreateWindowExW` with `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
  WS_EX_NOACTIVATE`
- `WS_EX_TRANSPARENT` — mouse clicks pass through to the window below
- `WS_EX_NOACTIVATE` — window cannot receive keyboard focus (important: the
  foreground app must keep focus so we can intercept its keystrokes via the hook)
- `SetLayeredWindowAttributes` or `UpdateLayeredWindow` for transparency

### Low-level keyboard hook
- `SetWindowsHookEx(WH_KEYBOARD_LL, hook_proc, NULL, 0)` — fires for all keystrokes
  system-wide, before the foreground app sees them
- Must be called from a thread that runs a message loop (`GetMessage` / `PeekMessage`)
- Return `CallNextHookEx(nCode, wParam, lParam)` to pass the key through;
  return a non-zero value to suppress it (suppress the hotkey keystroke itself)
- `UnhookWindowsHookEx` on shutdown

### DPI awareness
- Declare the process DPI-aware (`SetProcessDpiAwarenessContext`) early in `main`
- UIA `BoundingRectangle` returns physical pixels on DPI-aware processes — good
- All Win32 window coordinates must be in physical pixels — consistent with UIA

### SendInput for clicks
- Prefer `SendInput` over `mouse_event` (deprecated)
- Coordinates in `MOUSEINPUT` with `MOUSEEVENTF_ABSOLUTE` are in the range 0–65535
  normalised across the virtual screen (all monitors combined)
- Formula: `abs_x = (physical_x * 65535) / GetSystemMetrics(SM_CXVIRTUALSCREEN)`

---

## Decision log

Locked decisions. Do **not** re-evaluate these mid-build; revisit only if a
listed approach proves impossible.

- **2026-06-13 — Overlay renderer: WebView2** (via `webview2-com`). Chosen over
  raw GDI and Direct2D/`wgpu`. Rationale: HTML/CSS gives the most polished result
  and reuses existing Tauri/React experience. The cost (async init, transparency
  setup, heavier deps) is accepted up front so we never switch rendering tech
  later. GDI and Direct2D are rejected, not deferred.
- **2026-06-13 — `windows` crate pinned to 0.62** to match `webview2-com 0.39`'s
  re-exported version, so `HWND`/COM types interop across both crates.
- **2026-06-13 — WebView2 host mode: composition** (`CreateCoreWebView2Composition‑
  Controller` + DirectComposition + `WS_EX_NOREDIRECTIONBITMAP`). Verified against
  MS docs: controller mode **cannot** render see‑through transparency over the
  desktop; only composition mode can. Since the final overlay must be transparent,
  click‑through, and `WS_EX_NOACTIVATE`, we build composition mode from M2 to avoid
  re‑plumbing the overlay later. Requires D3D11 + DXGI + DirectComposition setup.
  Input forwarding (`SendMouseInput`/pointer) is deferred — the overlay is purely
  visual; keystrokes arrive via the global hook (M4).

---

## Milestone plan

| # | Name | Done? | Goal |
|---|---|---|---|
| M1 | UIA scan → stdout | ☑ | Rust can walk a window's UIA tree and print element positions |
| M2 | Layered window | ☑ | Transparent composition-mode WebView2 overlay appears and disappears on cue |
| M3 | Scan + overlay wired | ☑ | Hint labels render at real element positions |
| M4 | Keyboard hook + filtering | ☑ | Typing filters hints; match fires a click |
| M5 | Polish | ☐ | Tray icon, config file, right-click mode, DPI edge cases |

Work the milestones in order. Do not start M2 work inside an M1 session.

---

## Coding conventions

- **Rust edition**: 2021
- **Error handling**: Use `anyhow::Result` at the binary boundary (`main`, thread
  spawns). Use `windows::core::Result` inside `windows-rs` call sites. Propagate
  with `?`; avoid `.unwrap()` except in `main` where a panic is acceptable.
- **Unsafe**: All `windows-rs` calls require `unsafe`. Wrap each logical operation
  in a small `unsafe` block with a comment explaining why it's safe to call here.
  Do not write a single giant `unsafe` block spanning a whole function.
- **Naming**: Follow Rust conventions (`snake_case` functions, `PascalCase` types).
  Win32 constants (`WS_EX_LAYERED` etc.) keep their Win32 casing when referenced in
  comments; in code use the `windows` crate's constants directly.
- **No `std::thread::sleep` on the main thread.** The main thread runs the Windows
  message loop. Blocking it kills hotkey responsiveness.
- **COM hygiene**: Call `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` at the
  top of every thread that touches UIA or WebView2. Store COM interfaces in typed
  wrappers; do not pass raw pointers across thread boundaries.

---

## Known hard problems (do not attempt without a plan)

1. **Electron / WebView2 content**: UIA sees the host window but not DOM elements
   inside the webview. Mitigation: Chrome DevTools Protocol over a debug socket.
   Deferred to post-M5.

2. **UAC-elevated windows**: A non-elevated process cannot hook a window running as
   Administrator. The app may need to run elevated itself, or use UI Access (requires
   signed install to `Program Files`). Deferred.

3. **Multiple monitors with mixed DPI**: `SM_CXVIRTUALSCREEN` and per-monitor DPI
   must both be accounted for in coordinate translation. Handle during M5.

4. **WebView2 overlay transparency**: Solved via **composition mode** (see Decision
   log). Use `CreateCoreWebView2CompositionController`, host the WebView in a
   DirectComposition visual tree on a `WS_EX_NOREDIRECTIONBITMAP` window, and set
   `CoreWebView2Controller.DefaultBackgroundColor` alpha to 0. `UpdateLayeredWindow`
   does **not** work here — it can't paint WebView2's own rendering surface.

---

## Planned Features

Tracked in `TODO.md` in the project root — do not implement anything from it without explicit instruction.
Do not remove ANY items from `TODO.md` when task are complete - the user will manually remove items.

---

## Session Context Management

Long sessions degrade silently — context fills, important early details fall out of the window, and the session ends abruptly mid-implementation with no record of where things stand. This section governs how to handle that gracefully.

### Track context depth continuously

After completing each logical unit of work (a file, a function, a bug fix, a subagent invocation), pause and assess headroom. The session is approaching its limit when any of these are true:

- Multiple large files have been read or written this session
- Two or more subagent invocations have occurred
- The task still has significant work remaining and the conversation is already long
- You are about to start a new file or feature chunk and are uncertain you can finish it

When in doubt, stop early — an orderly handoff is always better than being cut off mid-function.

### Stop at a logical boundary

Never start a new file, function, or feature chunk without enough headroom to finish it. Complete the current atomic unit cleanly, then stop. A partial implementation left without documentation is worse than no implementation.

### Write RESUME.md before signaling the user

When stopping due to context pressure, write `RESUME.md` to the project root **before** telling the user. This file is the authoritative pickup document for the next session. Delete and rewrite it fresh each time — do not append to a previous version.

`RESUME.md` must contain all six sections below. Omitting any section defeats the purpose.

---

**1. Completed this session**
A concise, file-by-file list of every change made. Include the file path and a one-line description of what changed and why. If a subagent was used, note what it returned.

**2. Current state of in-progress work**
If anything was left unfinished, describe the exact file, the function or component, and the precise state it was left in. If nothing is in-progress, say so explicitly.

**3. Next steps — ordered and specific**
Not a summary. Actual instructions the next session can execute without re-reasoning. Each step should name the file, the action, and the expected outcome. Steps must be in dependency order.

**4. Decisions made this session**
Any architectural choices, tradeoffs, or "why we did it this way" notes that are not obvious from reading the code. The next session will not have this conversation's context — these notes replace it.

**5. Discoveries**
Bugs found but not yet fixed, edge cases identified, unexpected constraints, or anything that surprised you. Flag each with whether it blocks the next steps or can be deferred.

**6. Verification steps**
The exact commands the next session should run first to confirm the project is in the expected state before continuing (e.g. `npm run tauri dev`, a specific test, a build check).

---

### Signal the user

After writing `RESUME.md`, tell the user:

- That the session context is running low
- What was completed
- That `RESUME.md` has been written to the project root with full pickup instructions
- To start a new session, paste `RESUME.md` into the first message, and continue

Do not attempt any further implementation after this point. The next session will read `RESUME.md` and pick up from there.

---

## Agent Rules

### Plan before implementing
For any task touching more than two files, invoke the `planner` subagent before writing any code.
Do not begin implementation until the task list is returned and reviewed.

### Review after implementing
After completing any implementation task, invoke the `reviewer` subagent.
Pass it a one-paragraph description of what changed and why.
A task is not complete until the reviewer returns a clean report or all flagged issues are resolved.

The reviewer prompt must instruct the subagent to do all of the following — not just read the changed lines in isolation:

1. **Trace every call chain end-to-end.** For any new callback or event handler, follow the argument from the point it is created (e.g. a React synthetic event) through every wrapper function until it reaches the final consumer. Confirm each wrapper actually forwards the argument — `() => fn()` drops arguments silently while `(e) => fn(e)` does not.

2. **Check for undefined dereferences.** If a variable might be `undefined` at a call site (e.g. an event object passed through a wrapper that ignores its arguments), flag it as a bug even if it looks correct in isolation.

3. **Verify state consistency after every code path.** For new state variables, enumerate every path that modifies them and confirm the result is always consistent with dependent derived state or rendering logic.

4. **Read the files, do not reason from the summary alone.** The reviewer must read each changed file in full before reporting. Reasoning from a description without reading the code will miss implementation details.

### Keep subagents focused
Each subagent invocation must have a single, clearly scoped job.
Do not pass full conversation history to a subagent — summarize only what it needs.

### Model / cost discipline
- Subagent model: `claude-sonnet-4-6`
- Do not parallelize tasks that have dependencies — sequence them explicitly
- If a task requires reading more than 10 files, delegate the exploration to a subagent first and work from its summary


---

## Reference material

- Windows UI Automation docs: https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-uiautomationoverview
- `windows-rs` UIA bindings: https://microsoft.github.io/windows-rs/
- WebView2 transparency: https://learn.microsoft.com/en-us/microsoft-edge/webview2/
- Python prototype (reference behaviour): `prototype/winhint.py`
- Vimac (macOS reference app, open source): https://github.com/dexterleng/vimac