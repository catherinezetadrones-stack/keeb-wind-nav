#!/usr/bin/env python3
"""
winhint.py — Keyboard-driven UI navigation for Windows
A Vimac / Homerow-style hint overlay for the Windows desktop.

INSTALL
    pip install uiautomation pynput pywin32

RUN
    python winhint.py

USAGE
    Ctrl+Alt+Space  → activate hint mode
    type hint keys  → filter hints; auto-clicks on unique match
    Escape          → cancel / close overlay
    Ctrl+C          → quit winhint
"""
from __future__ import annotations
import sys, time, string, threading, itertools, ctypes
import tkinter as tk
import tkinter.font as tkfont
from typing import Optional

# ── dependency check ──────────────────────────────────────────────────────────
try:
    import uiautomation as auto
except ImportError:
    sys.exit("Missing: pip install uiautomation")

try:
    from pynput import keyboard as pynput_kb
    from pynput.keyboard import HotKey
except ImportError:
    sys.exit("Missing: pip install pynput")

try:
    import win32gui
except ImportError:
    sys.exit("Missing: pip install pywin32")


# ── configuration (edit freely) ───────────────────────────────────────────────
HOTKEY       = '<ctrl>+<alt>+space'  # Activation hotkey
HINT_CHARS   = string.ascii_lowercase  # Characters used to build hint labels
MAX_ELEMENTS = 200  # Cap — keeps scanning snappy on dense windows

# Visual theme (neon-on-dark)
BG_COL     = '#0d1117'  # Label background
BORDER_COL = '#00e5cc'  # Teal — untyped portion of hint
TYPED_COL  = '#ff9500'  # Orange — portion you've already typed
CLICK_MS   = 80         # ms between overlay close and the simulated click


# ── hint label generation ─────────────────────────────────────────────────────
def _hints(n: int) -> list[str]:
    """Return n unique short strings: a…z, aa, ab … (up to 702 labels)."""
    pool = list(HINT_CHARS)
    for a, b in itertools.product(HINT_CHARS, HINT_CHARS):
        pool.append(a + b)
        if len(pool) >= n:
            break
    return pool[:n]


# ── simulated left-click via Win32 ────────────────────────────────────────────
def _click(x: int, y: int) -> None:
    ctypes.windll.user32.SetCursorPos(x, y)
    time.sleep(0.02)
    ctypes.windll.user32.mouse_event(0x0002, 0, 0, 0, 0)  # MOUSEEVENTF_LEFTDOWN
    ctypes.windll.user32.mouse_event(0x0004, 0, 0, 0, 0)  # MOUSEEVENTF_LEFTUP


# ── Windows UI Automation scanner ─────────────────────────────────────────────
# UIA control types we consider "clickable"
_CLICKABLE_TYPES = frozenset([
    auto.ControlType.ButtonControl,
    auto.ControlType.CheckBoxControl,
    auto.ControlType.ComboBoxControl,
    auto.ControlType.EditControl,
    auto.ControlType.HyperlinkControl,
    auto.ControlType.ListItemControl,
    auto.ControlType.MenuItemControl,
    auto.ControlType.RadioButtonControl,
    auto.ControlType.TabItemControl,
    auto.ControlType.TreeItemControl,
    auto.ControlType.DataItemControl,
    auto.ControlType.HeaderItemControl,
    auto.ControlType.SplitButtonControl,
])


def scan(hwnd: int) -> list[tuple[int, int]]:
    """Return (cx, cy) screen-coords for every visible clickable element in hwnd."""
    out: list[tuple[int, int]] = []
    try:
        wl, wt, wr, wb = win32gui.GetWindowRect(hwnd)
        root = auto.ControlFromHandle(hwnd)
        if root:
            _walk(root, out, wl, wt, wr, wb)
    except Exception as exc:
        print(f"[scan] {exc}")
    return out


def _walk(ctrl, out: list, wl: int, wt: int, wr: int, wb: int, depth: int = 0):
    """Recursively walk the UIA tree, collecting clickable elements."""
    if depth > 12 or len(out) >= MAX_ELEMENTS:
        return
    try:
        r = ctrl.BoundingRectangle
        if not r or r.width() <= 0 or r.height() <= 0:
            return

        # Skip elements entirely outside the window rect
        cx, cy = int(r.xcenter()), int(r.ycenter())
        if not (wl <= cx <= wr and wt <= cy <= wb):
            return

        # Skip elements the OS reports as off-screen
        try:
            if ctrl.IsOffscreen:
                return
        except Exception:
            pass

        if ctrl.ControlType in _CLICKABLE_TYPES:
            # Deduplicate elements that land on the same pixel
            if not any(abs(e[0] - cx) < 4 and abs(e[1] - cy) < 4 for e in out):
                out.append((cx, cy))

        for child in ctrl.GetChildren():
            if len(out) >= MAX_ELEMENTS:
                break
            _walk(child, out, wl, wt, wr, wb, depth + 1)

    except Exception:
        pass  # Stale UIA handles are common; silently skip


# ── transparent hint overlay (tkinter) ───────────────────────────────────────
class Overlay:
    """
    Fullscreen, always-on-top, click-through overlay.

    Non-label areas use the transparent colour (black) so the underlying
    window is fully visible. Label rectangles are opaque.
    """

    def __init__(self, root: tk.Tk):
        self._root  = root
        self._win:   Optional[tk.Toplevel] = None
        self._cv:    Optional[tk.Canvas]   = None
        self._fnt:   Optional[tkfont.Font] = None
        self._items: dict[str, tuple[int, int]] = {}  # hint → (bg_id, txt_id)
        self._map:   dict[str, tuple[int, int]] = {}  # hint → (cx, cy)
        self._typed  = ''
        self.active  = False

    # ── public API ────────────────────────────────────────────────────────────
    def show(self, pts: list[tuple[int, int]]) -> None:
        """Display hint labels over pts. Must be called from the Tk main thread."""
        if self.active:
            self._teardown()
        if not pts:
            print("[winhint] No clickable elements found in foreground window.")
            return
        labels = _hints(len(pts))
        self._map   = {labels[i]: pts[i] for i in range(len(pts))}
        self._typed = ''
        self.active = True
        self._build()

    def dismiss(self) -> None:
        self._teardown()

    # ── private ───────────────────────────────────────────────────────────────
    def _build(self) -> None:
        sw = self._root.winfo_screenwidth()
        sh = self._root.winfo_screenheight()

        win = tk.Toplevel(self._root)
        win.title('__winhint_overlay__')
        win.geometry(f'{sw}x{sh}+0+0')
        win.overrideredirect(True)       # No title bar / decorations
        win.attributes('-topmost', True)
        win.configure(bg='black')
        win.attributes('-transparentcolor', 'black')  # Black = fully click-through
        win.attributes('-alpha', 0.97)

        cv = tk.Canvas(win, bg='black', highlightthickness=0, cursor='none')
        cv.pack(fill='both', expand=True)

        # Pick the best available monospace font
        available = tkfont.families()
        face = next(
            (f for f in ('Cascadia Code', 'Cascadia Mono', 'Consolas', 'Courier New')
             if f in available),
            'TkFixedFont'
        )
        fnt = tkfont.Font(family=face, size=11, weight='bold')

        self._items.clear()
        for hint, (cx, cy) in self._map.items():
            label = hint.upper()
            tw = fnt.measure(label) + 10
            th = 20

            bg = cv.create_rectangle(
                cx - tw // 2 - 1, cy - th // 2 - 1,
                cx + tw // 2 + 1, cy + th // 2 + 1,
                fill=BG_COL, outline=BORDER_COL, width=1
            )
            tx = cv.create_text(cx, cy, text=label, font=fnt, fill=BORDER_COL)
            self._items[hint] = (bg, tx)

        win.bind('<Key>', self._on_key)
        win.focus_force()   # Steal keyboard focus from foreground app
        win.grab_set()      # Capture all Tk events while overlay is shown

        self._win = win
        self._cv  = cv
        self._fnt = fnt

    def _on_key(self, ev: tk.Event) -> None:
        sym = ev.keysym.lower()

        if sym == 'escape':
            self._teardown()
            return

        if sym not in string.ascii_lowercase:
            return  # Ignore non-hint keys (Shift, Ctrl, arrows, etc.)

        self._typed += sym
        self._refresh()

        # Exact match → click
        if self._typed in self._map:
            cx, cy = self._map[self._typed]
            self._teardown()
            # Small delay so the overlay has time to close before clicking
            self._root.after(CLICK_MS, lambda: _click(cx, cy))
            return

        # Nothing left matching → give up
        if not any(h.startswith(self._typed) for h in self._map):
            self._teardown()

    def _refresh(self) -> None:
        """Hide non-matching hints; colour the typed prefix orange on matches."""
        for hint, (bg_id, tx_id) in self._items.items():
            if hint.startswith(self._typed):
                typed = self._typed.upper()
                rest  = hint[len(self._typed):].upper()
                self._cv.itemconfig(tx_id,
                                    text=typed + rest,
                                    fill=TYPED_COL if typed else BORDER_COL)
                self._cv.itemconfig(bg_id, state='normal')
                self._cv.itemconfig(tx_id, state='normal')
            else:
                self._cv.itemconfig(bg_id, state='hidden')
                self._cv.itemconfig(tx_id, state='hidden')

    def _teardown(self) -> None:
        self.active = False
        if self._win:
            try:
                self._win.grab_release()
                self._win.destroy()
            except Exception:
                pass
            self._win = None


# ── application ───────────────────────────────────────────────────────────────
class WinHint:
    def __init__(self):
        # Hidden controller window — tkinter requires a root Tk() to exist
        self._root    = tk.Tk()
        self._root.withdraw()
        self._overlay = Overlay(self._root)

    def _on_hotkey(self) -> None:
        """
        Called from pynput's background listener thread.
        Never touch tkinter here — schedule everything via root.after().
        """
        hwnd = win32gui.GetForegroundWindow()  # Capture now, before overlay opens
        if self._overlay.active:
            self._root.after(0, self._overlay.dismiss)
        else:
            self._root.after(0, lambda: self._launch(hwnd))

    def _launch(self, hwnd: int) -> None:
        """Start a background scan, then hand results to the overlay."""
        def _bg():
            pts = scan(hwnd)
            self._root.after(0, lambda: self._overlay.show(pts))

        threading.Thread(target=_bg, daemon=True).start()

    def run(self) -> None:
        hk = HotKey(HotKey.parse(HOTKEY), self._on_hotkey)

        listener = pynput_kb.Listener(
            on_press   = lambda k: hk.press(k)   if k else None,
            on_release = lambda k: hk.release(k) if k else None,
        )
        listener.start()

        print(f"WinHint running  —  {HOTKEY} to activate  |  Esc to cancel  |  Ctrl+C to quit")
        try:
            self._root.mainloop()
        finally:
            listener.stop()


# ─────────────────────────────────────────────────────────────────────────────
if __name__ == '__main__':
    WinHint().run()
