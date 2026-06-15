//! Pure pane-boundary (splitter) geometry.
//!
//! Native apps draw the splitter between two panes (e.g. a nav pane and a
//! content pane) but do **not** expose it as a UIA element — so it can't be
//! found or resized via the accessibility tree. The panes themselves *are*
//! visible, though, and two adjacent panes share an edge: that shared-edge pixel
//! *is* the draggable splitter. This module turns a set of pane rectangles into
//! a deduplicated set of splitter "boundary lines" we can later drag with a
//! simulated mouse drag (see `click::drag`).
//!
//! No Win32 calls live here — just `RECT` arithmetic — so the logic is fully
//! unit-testable (mirrors `resize.rs`). The caller supplies pane rects from UIA
//! and consumes `Boundary`s.

use windows::Win32::Foundation::RECT;

/// Smallest pane (each axis, physical px) considered for adjacency. Filters out
/// toolbars/labels masquerading as panes. Kept equal to `scanner`'s pane filter.
pub const MIN_PANE_SIZE: i32 = 80;
/// Two pane edges within this many px (on the boundary axis) count as adjacent —
/// covers the small gap real splitters leave (e.g. Explorer's nav/content edges
/// sit at 373 vs 377 = 4px apart).
pub const EDGE_TOLERANCE: i32 = 8;
/// Minimum cross-axis overlap (px) for a shared edge to be a real splitter —
/// filters incidental near-touches between barely-overlapping panes.
pub const MIN_OVERLAP: i32 = 40;
/// Two same-orientation boundaries whose `coord`s are within this (and whose
/// spans overlap) are treated as one — collapses the many duplicate/nested panes
/// that share the same edge into a single splitter.
pub const DEDUP_COORD: i32 = 8;

/// A splitter bar's orientation, named for the *bar*, not the drag direction:
/// a `Vertical` bar runs top-to-bottom and is dragged horizontally (changes x);
/// a `Horizontal` bar runs left-to-right and is dragged vertically (changes y).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

/// One splitter line between two adjacent panes. `coord` is the line's position
/// on its perpendicular axis (x for `Vertical`, y for `Horizontal`); the span is
/// the overlapping extent of the two panes along the line (y-range for
/// `Vertical`, x-range for `Horizontal`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Boundary {
    pub orientation: Orientation,
    pub coord: i32,
    pub span_start: i32,
    pub span_end: i32,
}

/// Detect splitter boundaries from a set of pane rects: every pair of panes
/// whose edges nearly touch (within `EDGE_TOLERANCE`) with enough cross-axis
/// overlap (`MIN_OVERLAP`) yields a boundary on their shared edge. The result is
/// deduplicated (nested/duplicate panes collapse) and sorted for determinism.
pub fn find_boundaries(panes: &[RECT]) -> Vec<Boundary> {
    let panes: Vec<RECT> = panes
        .iter()
        .copied()
        .filter(|r| (r.right - r.left) >= MIN_PANE_SIZE && (r.bottom - r.top) >= MIN_PANE_SIZE)
        .collect();

    let mut candidates: Vec<Boundary> = Vec::new();
    // Ordered pairs: each adjacency surfaces once, as the pair where `a` is the
    // left/top pane. The reverse ordering checks the opposite (non-touching)
    // edges, so there is no double-count from direction alone.
    for (i, a) in panes.iter().enumerate() {
        for (j, b) in panes.iter().enumerate() {
            if i == j {
                continue;
            }
            // Vertical bar: a's right edge meets b's left edge.
            if (a.right - b.left).abs() <= EDGE_TOLERANCE {
                let start = a.top.max(b.top);
                let end = a.bottom.min(b.bottom);
                if end - start >= MIN_OVERLAP {
                    candidates.push(Boundary {
                        orientation: Orientation::Vertical,
                        coord: (a.right + b.left) / 2,
                        span_start: start,
                        span_end: end,
                    });
                }
            }
            // Horizontal bar: a's bottom edge meets b's top edge.
            if (a.bottom - b.top).abs() <= EDGE_TOLERANCE {
                let start = a.left.max(b.left);
                let end = a.right.min(b.right);
                if end - start >= MIN_OVERLAP {
                    candidates.push(Boundary {
                        orientation: Orientation::Horizontal,
                        coord: (a.bottom + b.top) / 2,
                        span_start: start,
                        span_end: end,
                    });
                }
            }
        }
    }

    dedup(candidates)
}

/// Collapse near-identical boundaries (same orientation, `coord`s within
/// `DEDUP_COORD`, overlapping spans) into one, unioning their spans. Returns a
/// stable order: vertical bars before horizontal, then by `coord`.
fn dedup(mut candidates: Vec<Boundary>) -> Vec<Boundary> {
    candidates.sort_by_key(|b| (orient_key(b.orientation), b.coord, b.span_start));

    let mut out: Vec<Boundary> = Vec::new();
    for cand in candidates {
        if let Some(last) = out.last_mut() {
            let near = last.orientation == cand.orientation
                && (last.coord - cand.coord).abs() <= DEDUP_COORD
                && spans_overlap(last, &cand);
            if near {
                // Union the spans; keep the existing coord (they're within
                // DEDUP_COORD of each other, so any is representative).
                last.span_start = last.span_start.min(cand.span_start);
                last.span_end = last.span_end.max(cand.span_end);
                continue;
            }
        }
        out.push(cand);
    }
    out
}

/// Do two boundaries' spans overlap by at least `MIN_OVERLAP`? Used to decide
/// whether two same-line candidates are the *same* splitter (merge) or two
/// distinct splitters stacked along one line (keep both).
fn spans_overlap(a: &Boundary, b: &Boundary) -> bool {
    let start = a.span_start.max(b.span_start);
    let end = a.span_end.min(b.span_end);
    end - start >= MIN_OVERLAP
}

/// Sort key making `Vertical` bars order before `Horizontal` ones.
fn orient_key(o: Orientation) -> u8 {
    match o {
        Orientation::Vertical => 0,
        Orientation::Horizontal => 1,
    }
}

/// The point to render the handle on, and to start the mouse drag from: the
/// middle of the boundary's span, on the boundary line.
pub fn drag_point(b: &Boundary) -> (i32, i32) {
    let mid = (b.span_start + b.span_end) / 2;
    match b.orientation {
        Orientation::Vertical => (b.coord, mid),
        Orientation::Horizontal => (mid, b.coord),
    }
}

/// A copy of `b` with its line moved by the drag delta. Only the component along
/// the drag axis matters: `Vertical` bars take `dx`, `Horizontal` bars take `dy`
/// — so the caller can pass a full `(dx, dy)` and let orientation pick the axis.
pub fn apply_drag(b: &Boundary, dx: i32, dy: i32) -> Boundary {
    let mut out = *b;
    match b.orientation {
        Orientation::Vertical => out.coord += dx,
        Orientation::Horizontal => out.coord += dy,
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

    #[test]
    fn explorer_nav_content_splitter() {
        // Real File Explorer geometry: nav pane right edge 373, content pane left
        // edge 377 (4px gap), both spanning y 290..913 → one vertical splitter.
        let nav = rect(94, 290, 373, 913);
        let content = rect(377, 290, 1122, 913);
        let b = find_boundaries(&[nav, content]);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].orientation, Orientation::Vertical);
        assert_eq!(b[0].coord, 375); // (373 + 377) / 2
        assert_eq!(b[0].span_start, 290);
        assert_eq!(b[0].span_end, 913);
        assert_eq!(drag_point(&b[0]), (375, 601)); // (290 + 913) / 2 = 601
    }

    #[test]
    fn no_panes_no_boundaries() {
        assert!(find_boundaries(&[]).is_empty());
    }

    #[test]
    fn single_pane_no_boundary() {
        assert!(find_boundaries(&[rect(0, 0, 400, 400)]).is_empty());
    }

    #[test]
    fn tiny_pane_is_filtered() {
        // A skinny strip (below MIN_PANE_SIZE wide) adjacent to a real pane must
        // not produce a boundary — it's filtered before adjacency analysis.
        let tiny = rect(0, 0, 20, 400); // 20px wide < MIN_PANE_SIZE
        let big = rect(24, 0, 500, 400);
        assert!(find_boundaries(&[tiny, big]).is_empty());
    }

    #[test]
    fn duplicate_panes_collapse_to_one() {
        // Two copies of the same nav/content pair (as nested panes produce) must
        // dedup to a single boundary.
        let nav = rect(94, 290, 373, 913);
        let content = rect(377, 290, 1122, 913);
        let b = find_boundaries(&[nav, content, nav, content]);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn insufficient_overlap_no_boundary() {
        // Edges within tolerance but cross-axis overlap < MIN_OVERLAP.
        let a = rect(0, 0, 200, 200);
        let b = rect(200, 180, 400, 400); // overlap y 180..200 = 20 < MIN_OVERLAP
        assert!(find_boundaries(&[a, b]).is_empty());
    }

    #[test]
    fn horizontal_boundary_between_stacked_panes() {
        // Top pane's bottom edge meets bottom pane's top edge → horizontal bar.
        let top = rect(0, 0, 400, 300);
        let bottom = rect(0, 304, 400, 600); // 4px gap on the y axis
        let bs = find_boundaries(&[top, bottom]);
        assert_eq!(bs.len(), 1);
        assert_eq!(bs[0].orientation, Orientation::Horizontal);
        assert_eq!(bs[0].coord, 302); // (300 + 304) / 2
        assert_eq!(bs[0].span_start, 0);
        assert_eq!(bs[0].span_end, 400);
        assert_eq!(drag_point(&bs[0]), (200, 302));
    }

    #[test]
    fn negative_coords_multi_monitor() {
        // Panes on a monitor left of the primary (negative x) still resolve.
        let nav = rect(-500, 0, -300, 400);
        let content = rect(-296, 0, 0, 400); // 4px gap
        let b = find_boundaries(&[nav, content]);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].orientation, Orientation::Vertical);
        assert_eq!(b[0].coord, -298); // (-300 + -296) / 2
    }

    #[test]
    fn apply_drag_moves_only_the_drag_axis() {
        let v = Boundary {
            orientation: Orientation::Vertical,
            coord: 375,
            span_start: 290,
            span_end: 913,
        };
        // Vertical bar takes dx, ignores dy.
        assert_eq!(apply_drag(&v, 8, 99).coord, 383);
        let h = Boundary {
            orientation: Orientation::Horizontal,
            coord: 302,
            span_start: 0,
            span_end: 400,
        };
        // Horizontal bar takes dy, ignores dx.
        assert_eq!(apply_drag(&h, 99, -8).coord, 294);
    }

    #[test]
    fn two_distinct_splitters_on_one_line_kept() {
        // Two vertical bars at the same x but non-overlapping spans (panes
        // stacked above/below a third on the right) must NOT merge.
        let left_top = rect(0, 0, 200, 200);
        let right_top = rect(204, 0, 400, 200);
        let left_bot = rect(0, 400, 200, 600);
        let right_bot = rect(204, 400, 400, 600);
        let bs = find_boundaries(&[left_top, right_top, left_bot, right_bot]);
        // Same coord (~202), two disjoint y-spans (0..200 and 400..600).
        assert_eq!(bs.len(), 2);
        assert!(bs.iter().all(|b| b.orientation == Orientation::Vertical));
    }
}
