# TODO.md — Keeb-Wind-Nav

## Working Through This List

Work through one section per session, not the full list at once. Start each session with:
Work through the [section name] section of TODO.md only.
Use the planner subagent first, then implement and review each task
before moving to the next. Stop when all items in the section are done
and summarize what changed.

**Session rules:**
- Always invoke the planner subagent before writing any code
- Always invoke the reviewer subagent after each task is complete
- A task is not done until the reviewer returns a clean report or all flagged issues are resolved
- Do not move to the next task until the current one is verified

---

## UI / UX Bugs

## Firmware Bugs


## Planned Features

### App UI / UX Improvements

_Straightforward changes to the existing interface_

### New App Features

_More substantial additions to the app itself_

#### Inner pane / panel resize — PARKED (do not continue without a fresh plan)

The window-edge resize (handles a–h) works well. The **pane-splitter** extension
(handles i–z, drag the boundary between two adjacent panes) is built, tested, and
compiles, but **does not actually move real panels** in practice — it is parked,
not deleted.

What we observed (2026-06-14):
- The one Windows panel the user actually wanted to move **did not show up as a
  splitter handle at all** — `scanner::collect_panes` + `splitter::find_boundaries`
  didn't surface it (likely not two adjacent `Pane`-typed UIA elements, or the
  edge/overlap thresholds filtered it out).
- Where a splitter handle *did* appear, dragging it (`click::drag` on the shared
  edge) **moved only our overlay hint, not the underlying panel** — the simulated
  mouse drag isn't landing on a real draggable splitter.

Why it's hard (revisit notes for next attempt):
- Splitters are not UIA elements; the "two adjacent panes share a draggable edge"
  assumption holds for some apps (Explorer-style) but not the target case here.
- Real splitters may sit on a control that isn't a `Pane`, may need a hover/grab
  affordance before the drag registers, or may require a real press-hold-move with
  timing (not a synthesized burst) to be recognized.
- Likely needs per-app handling and deeper UIA-tree work; "more depths and other
  handling to work out" before it's worth shipping.

Current code state (all uncommitted, builds clean, 33 tests pass):
- `splitter.rs` (pure geometry), `scanner::collect_panes`/`pane_walk`,
  `click::drag`, and the app.rs/overlay.rs wiring (`ResizeSelection`, blue i–z
  handles) are all in place. Keep them; they're the foundation for a future retry.
- See `RESIZE_PLAN.md` for the full design history.

### Firmware related App Features

_Require knowledge about firmware and may require web access for research_


## Distribution & Packaging

_Goal: install and run this on the user's other computer as an everyday tool._

- **Installer.** Produce a proper Windows installer for `winhint.exe` (optional
  run-at-startup). Decide tooling (e.g. WiX/MSI, Inno Setup, or MSIX).
- **System tray icon, not a taskbar window.** Run as a background tray app with a
  minimal notification-area icon (right-click menu: pause/resume, quit, maybe open
  config). No full taskbar / Alt-Tab presence (the overlay window is already
  `WS_EX_TOOLWINDOW`).
- **Publisher identity + code signing.** The installer/exe should show the user as
  the publisher (nice display name) and be **signed** so SmartScreen/Defender
  don't flag it. Needs: a code-signing certificate (self-signed is free but still
  warns; an OV/EV cert avoids SmartScreen warnings but costs money) and a chosen
  publisher display name. CONFIRM cert availability + exact publisher name before
  starting.


## Repo & Publishing (GitHub)

_Goal: publish a clean, best-practices open-source repo._

- **MIT license.** Add a `LICENSE` (MIT) with the user as copyright holder, so the
  user owns the app and others may fork with attribution. Confirm the exact
  copyright name/year to use.
- **Contributors / authorship.** Primary author `czd` (GitHub
  `catherinezetadrones-stack`); secondary account `phbronson` (GitHub
  `phbronson999`). Keep commit authorship as `czd` per project rule; never add
  Claude as co-author.
- **README with visuals.** Use the `/repo-visuals` skill to produce screen
  recordings / hero visuals of the hint-mode navigation for the `README.md`:
  - Use a **clean desktop** for the Windows-app examples.
  - Use a website (GitHub) and/or VS Code for the other examples.
- **Repo hygiene.** Follow open-source best practices (README, LICENSE,
  `.gitignore` already present, maybe CONTRIBUTING, release build instructions).


## Tooling & Workflow

- **Transferable workflow skill.** This repo is the prototype for the user's other
  passion projects. Create a reusable skill capturing this workflow (TODO-driven,
  planner→implement→reviewer loop, RESUME.md handoff, commit/authorship rules) so
  the same process applies across all projects.
