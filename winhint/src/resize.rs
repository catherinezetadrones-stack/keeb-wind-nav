//! Pure window-resize geometry: handle positions and edge-drag math.
//!
//! No Win32 calls live here — just `RECT` arithmetic — so the logic is fully
//! unit-testable. The caller (`app.rs`) supplies/consumes `RECT`s and performs
//! the actual `SetWindowPos`.

use windows::Win32::Foundation::RECT;

/// Minimum window size enforced while resizing (physical pixels).
pub const MIN_WIDTH: i32 = 200;
/// Minimum window height enforced while resizing (physical pixels).
pub const MIN_HEIGHT: i32 = 120;

/// One of the eight drag handles: four corners and four edge midpoints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    /// All eight handles in a stable order — the caller assigns labels a–h by
    /// index, so this order must not change.
    pub fn all() -> [Handle; 8] {
        use Handle::*;
        [
            TopLeft,
            Top,
            TopRight,
            Right,
            BottomRight,
            Bottom,
            BottomLeft,
            Left,
        ]
    }

    /// Which edges this handle moves, as `(left, top, right, bottom)`. A corner
    /// moves two edges; an edge midpoint moves one. The opposite edges stay put.
    fn edges(self) -> (bool, bool, bool, bool) {
        use Handle::*;
        match self {
            TopLeft => (true, true, false, false),
            Top => (false, true, false, false),
            TopRight => (false, true, true, false),
            Right => (false, false, true, false),
            BottomRight => (false, false, true, true),
            Bottom => (false, false, false, true),
            BottomLeft => (true, false, false, true),
            Left => (true, false, false, false),
        }
    }
}

/// Screen position `(x, y)` of each handle for `r`, paired with the handle.
/// Corners sit on the rect's corners; midpoints at the centers of each edge.
pub fn handle_positions(r: RECT) -> [(Handle, i32, i32); 8] {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    use Handle::*;
    [
        (TopLeft, r.left, r.top),
        (Top, cx, r.top),
        (TopRight, r.right, r.top),
        (Right, r.right, cy),
        (BottomRight, r.right, r.bottom),
        (Bottom, cx, r.bottom),
        (BottomLeft, r.left, r.bottom),
        (Left, r.left, cy),
    ]
}

/// Move `handle`'s edge(s) by `(dx, dy)`, clamped so the rect never shrinks
/// below `(min_w, min_h)`. The opposite (fixed) edges stay in place; a moved
/// edge that would cross the minimum is pushed back to exactly the minimum.
pub fn apply_handle_move(
    r: RECT,
    handle: Handle,
    dx: i32,
    dy: i32,
    min_w: i32,
    min_h: i32,
) -> RECT {
    let (ml, mt, mr, mb) = handle.edges();
    let mut out = r;
    if ml {
        out.left += dx;
    }
    if mr {
        out.right += dx;
    }
    if mt {
        out.top += dy;
    }
    if mb {
        out.bottom += dy;
    }

    // Clamp to the minimum size by pushing the *moved* edge back toward the
    // fixed opposite edge.
    if ml && out.right - out.left < min_w {
        out.left = out.right - min_w;
    }
    if mr && out.right - out.left < min_w {
        out.right = out.left + min_w;
    }
    if mt && out.bottom - out.top < min_h {
        out.top = out.bottom - min_h;
    }
    if mb && out.bottom - out.top < min_h {
        out.bottom = out.top + min_h;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }
    /// Compare a rect to expected (left, top, right, bottom) without relying on
    /// RECT's own equality.
    fn assert_rect(a: RECT, l: i32, t: i32, r: i32, b: i32) {
        assert_eq!((a.left, a.top, a.right, a.bottom), (l, t, r, b));
    }

    #[test]
    fn right_edge_grows() {
        let out = apply_handle_move(rect(0, 0, 400, 300), Handle::Right, 50, 0, MIN_WIDTH, MIN_HEIGHT);
        assert_rect(out, 0, 0, 450, 300);
    }

    #[test]
    fn bottom_right_corner_moves_two_edges() {
        let out = apply_handle_move(
            rect(0, 0, 400, 300),
            Handle::BottomRight,
            20,
            30,
            MIN_WIDTH,
            MIN_HEIGHT,
        );
        assert_rect(out, 0, 0, 420, 330);
    }

    #[test]
    fn top_left_corner_moves_origin() {
        let out = apply_handle_move(
            rect(100, 100, 500, 400),
            Handle::TopLeft,
            -10,
            -20,
            MIN_WIDTH,
            MIN_HEIGHT,
        );
        assert_rect(out, 90, 80, 500, 400);
    }

    #[test]
    fn left_edge_clamps_to_min_width() {
        // Drag left edge right by 300 → width would be 100 < 200 → clamp.
        let out = apply_handle_move(rect(0, 0, 400, 300), Handle::Left, 300, 0, MIN_WIDTH, MIN_HEIGHT);
        assert_eq!(out.right, 400, "fixed edge stays");
        assert_eq!(out.right - out.left, MIN_WIDTH, "clamped to min width");
    }

    #[test]
    fn top_edge_clamps_to_min_height() {
        // Drag top edge down by 250 → height would be 50 < 120 → clamp.
        let out = apply_handle_move(rect(0, 0, 400, 300), Handle::Top, 0, 250, MIN_WIDTH, MIN_HEIGHT);
        assert_eq!(out.bottom, 300, "fixed edge stays");
        assert_eq!(out.bottom - out.top, MIN_HEIGHT, "clamped to min height");
    }

    #[test]
    fn positions_lie_on_rect() {
        let p = handle_positions(rect(10, 20, 110, 220));
        assert!(p.contains(&(Handle::TopLeft, 10, 20)));
        assert!(p.contains(&(Handle::BottomRight, 110, 220)));
        assert!(p.contains(&(Handle::Top, 60, 20))); // midpoint of top edge
        assert!(p.contains(&(Handle::Left, 10, 120))); // midpoint of left edge
    }

    #[test]
    fn all_returns_eight_distinct_handles() {
        let a = Handle::all();
        assert_eq!(a.len(), 8);
        for (i, h) in a.iter().enumerate() {
            assert!(!a[..i].contains(h), "duplicate handle in all()");
        }
    }
}
