# RESUME — WinHint productionization (publish-ready; next: transferable workflow skill)

The app is feature-complete for everyday use and essentially publish-ready.
This session productionized it. **Next task: build the "transferable workflow
skill"** (the last actionable roadmap item). Everything below is committed on
`master` unless noted.

## 1. Completed this session

All committed on `master` (newest first):
- **`a172a5b` Animated README hero.** `assets/hero.gif` (~10s, 644 KB) + README
  embed (top, `<p align="center">`). Built with the `/repo-visuals` skill. Shows a
  dark-mode GitHub page for `catherinezetadrones-stack/keeb-wind-nav`: tap
  CapsLock → command bar at top of screen (cyan BOTH badge) → ~24 cyan hint labels
  swarm every clickable element → BOTH-mode search `co`→`code` narrows a results
  list → clicks the green Code button → loops. Source/scratch lives OUTSIDE the
  repo at `C:\Users\phill\OneDrive\Desktop\repo-visuals-work\keeb-wind-nav\`
  (`index.html`, `capture2x.js`, `critique.js`, `frames/`, `hero.gif`).
- **`e01fb12` Inno Setup installer.** `installer/winhint.iss`. Per-user install to
  `%LOCALAPPDATA%\Programs\WinHint` (deliberate: keeps the exe dir writable for
  WebView2's `winhint.exe.WebView2` data folder, and no UAC). Publisher = Phillip
  L. Bronson, optional start-at-sign-in task, Start-menu shortcut,
  `AppMutex=WinHint_SingleInstance`, WebView2-runtime check. **Unsigned** (signing
  deferred). Compiles with Inno 6.7 → `installer/Output/WinHint-Setup-0.1.0.exe`
  (output dir gitignored).
- **`7738940` MIT LICENSE + README.** `LICENSE` (MIT, © 2026 Phillip L. Bronson).
  `README.md` (overview, features, how-it-works, build/run, keybindings table,
  requirements, known limitations). Roadmap section intentionally removed by the
  user (finish everything before publishing).
- **`fae405a` System-tray icon.** New `winhint/src/tray.rs` (Shell_NotifyIcon,
  stock `IDI_APPLICATION` icon, right-click menu: Pause/Resume + Quit). `PAUSED`
  atomic + `set_paused`/`is_paused` in `hotkey.rs` (paused = all keys pass through
  except Ctrl+Alt+Q). `WM_APP_TRAY` (=WM_APP+10) + `WM_COMMAND` routing in
  `app.rs`; tray added on startup (non-fatal) / removed on every exit path; pausing
  mid-session tears down any open overlay. `Win32_UI_Shell` Cargo feature.
  planner→reviewer both run; reviewer-clean after fixing the one flagged item.
- **`423d754` (prior) Window resize + parked splitter.** Already documented.

Uncommitted: `TODO.md` has doc-only edits (user's "rename the app" backlog item +
my "polish the hero later" note). `keeb-wind-nav.code-workspace` is intentionally
untracked. Memory files updated (`winhint-project.md`, `publishing-and-identity.md`).

## 2. Current state of in-progress work

**Nothing half-done.** All code compiles; 33 tests pass; the installer compiles;
the hero is committed. The repo-visuals skill run completed (Phase 5 — hero
placed). The only "in progress" item is the not-yet-started transferable skill.

## 3. Next steps — ordered

1. **Build the transferable workflow skill** (user's words: "create a skill that is
   transferable to my other projects so I follow the same workflow on all of the
   passion projects"). START WITH A SCOPING QUESTION — the form isn't pinned down.
   The workflow to capture is THIS repo's conventions: TODO-driven (work one
   `TODO.md` section per session), **planner subagent before any code**, **reviewer
   subagent after**, **RESUME.md handoff** under context pressure, **commit
   authorship = czd, never add Claude as co-author, commit only after user
   validation**, and the CLAUDE.md structure. Likely shape: a Claude Code skill
   (`SKILL.md`) that scaffolds these into a new project (CLAUDE.md template +
   TODO.md template + planner/reviewer agent defs + RESUME protocol). Confirm with
   the user: a *scaffolding* skill vs a *documentation/enforcement* skill, and
   where it should live (`~/.claude/skills/`?).
2. **(Deferred) Code signing** — blocked until the user has a cert (self-signed vs
   OV/EV). When ready: wire `signtool` into the installer build.
3. **(Backlog) App rename** — user flagged "winhint is terrible." Ripples into
   README, `installer/winhint.iss` (AppName/publisher/paths), the
   `Local\WinHint_SingleInstance` mutex (`main.rs`), and the hero. Do this BEFORE
   final publish + before re-shooting the hero.
4. **(Backlog) Hero polish** — dim the non-selected `QC` label at the click moment,
   tune typing pace / label density, maybe add a VS Code example. Re-run from the
   scratch dir (toolchain already installed — see §5).

## 4. Decisions made this session

- **Tray pause** lives as a `PAUSED` atomic in `hotkey.rs` (hook reads it
  cross-thread); pausing suppresses everything except Ctrl+Alt+Q and tears down any
  open overlay session.
- **Installer is per-user**, not Program Files — avoids UAC and the
  WebView2-in-read-only-dir failure. Unsigned for now (user chose to defer signing).
- **Hero = animated GIF**, dark-mode GitHub backdrop, BOTH-mode search demo, owner
  `catherinezetadrones-stack` (primary account). Static was rejected by the user.
- **Commit cadence:** the user commits per finished task after validating. Author
  is `czd`; no Claude co-author (project rule).

## 5. Discoveries / gotchas

- `cargo test` does NOT rebuild `winhint.exe`; stop a running daemon
  (`Get-Process winhint | Stop-Process -Force`) before `cargo build`. Single-
  instance guard is live.
- **repo-visuals toolchain is installed** for future hero work: puppeteer + its
  Chromium live under `C:\Users\phill\.claude\plugins\cache\livlign\repo-visuals\0.1.5\node_modules`
  (node resolves it via upward lookup); portable **ffmpeg** at
  `...\0.1.5\skills\repo-visuals\bin\ffmpeg.exe`. Chromium ignores
  `deviceScaleFactor` during screencast on this machine — use the zoom-2 fallback
  (`capture2x.js` in the scratch dir already does this) to get true 2× frames.
- Inno Setup `x64compatible` needs Inno 6.3+ (6.7 is installed).

## 6. Verification steps (run first next session)

```powershell
cd winhint
cargo test            # expect 33 passed
cargo build           # clean, no warnings
cd ..
git -C . log --oneline -6   # see the six commits listed in §1
git -C . status --short     # expect only: M TODO.md, ?? keeb-wind-nav.code-workspace
```
Confirm `winhint/src/tray.rs`, `installer/winhint.iss`, `LICENSE`, `README.md`,
and `assets/hero.gif` are present and committed.
