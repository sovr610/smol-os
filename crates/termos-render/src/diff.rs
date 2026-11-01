// SPDX-License-Identifier: GPL-3.0-or-later
//! Output damage: the minimal set of cell runs that changed between frames.

use serde::{Deserialize, Serialize};

use crate::cell::Cell;
use crate::grid::Grid;

/// A run of consecutive cells on one row, starting at (`col`, `row`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub row: u16,
    pub col: u16,
    pub cells: Vec<Cell>,
}

/// Unchanged cells between two changed runs are re-sent rather than starting
/// a new span when the gap is at most this long: a cursor move costs more
/// bytes than a few cells.
const MERGE_GAP: u16 = 4;

/// The spans that turn `prev` into `next`. If the sizes differ, every row is
/// sent in full.
pub fn diff(prev: &Grid, next: &Grid) -> Vec<Span> {
    if prev.cols() != next.cols() || prev.rows() != next.rows() {
        return full(next);
    }
    let mut spans = Vec::new();
    for y in 0..next.rows() {
        let (a, b) = (prev.row(y), next.row(y));
        if a == b {
            continue;
        }
        let mut x = 0u16;
        let cols = next.cols();
        while x < cols {
            if a[usize::from(x)] == b[usize::from(x)] {
                x += 1;
                continue;
            }
            // Never start on the trailing half of a wide character.
            let mut start = x;
            if start > 0 && b[usize::from(start)].is_continuation() {
                start -= 1;
            }
            let mut end = x + 1; // exclusive
            let mut gap = 0u16;
            let mut probe = end;
            while probe < cols {
                if a[usize::from(probe)] == b[usize::from(probe)] {
                    gap += 1;
                    if gap > MERGE_GAP {
                        break;
                    }
                } else {
                    gap = 0;
                    end = probe + 1;
                }
                probe += 1;
            }
            // Include the continuation of a trailing wide character.
            if end < cols && b[usize::from(end)].is_continuation() {
                end += 1;
            }
            spans.push(Span {
                row: y,
                col: start,
                cells: b[usize::from(start)..usize::from(end)].to_vec(),
            });
            x = end;
        }
    }
    spans
}

/// Every row of `grid` as one span each (initial frame, resize, redraw).
pub fn full(grid: &Grid) -> Vec<Span> {
    (0..grid.rows())
        .map(|y| Span {
            row: y,
            col: 0,
            cells: grid.row(y).to_vec(),
        })
        .collect()
}

/// Writes `spans` into `grid` (the client-side mirror of the screen).
pub fn apply(grid: &mut Grid, spans: &[Span]) {
    for span in spans {
        for (i, cell) in span.cells.iter().enumerate() {
            let Ok(offset) = u16::try_from(i) else { break };
            if let Some(slot) = grid.get_mut(span.col.saturating_add(offset), span.row) {
                *slot = cell.clone();
            }
        }
    }
}

/// Number of cells covered by `spans`.
pub fn cell_count(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.cells.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Style;

    #[test]
    fn identical_grids_produce_no_spans() {
        let g = Grid::new(10, 3);
        assert!(diff(&g, &g.clone()).is_empty());
    }

    #[test]
    fn one_changed_cell_is_one_cell_span() {
        let a = Grid::new(10, 3);
        let mut b = a.clone();
        b.put_str(4, 1, "x", Style::new(), 10);
        let spans = diff(&a, &b);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            (spans[0].row, spans[0].col, spans[0].cells.len()),
            (1, 4, 1)
        );
    }

    #[test]
    fn close_changes_merge_and_far_ones_do_not() {
        let a = Grid::new(30, 1);
        let mut b = a.clone();
        b.put_str(0, 0, "a", Style::new(), 30);
        b.put_str(3, 0, "b", Style::new(), 30);
        b.put_str(20, 0, "c", Style::new(), 30);
        let spans = diff(&a, &b);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].col, spans[0].cells.len()), (0, 4));
        assert_eq!((spans[1].col, spans[1].cells.len()), (20, 1));
    }

    #[test]
    fn wide_characters_are_never_split() {
        let mut a = Grid::new(6, 1);
        a.put_str(0, 0, "ab界", Style::new(), 6);
        let mut b = a.clone();
        // Change only the style of the wide char's continuation cell.
        if let Some(c) = b.get_mut(3, 0) {
            c.style = Style::new().bold();
        }
        let spans = diff(&a, &b);
        assert_eq!(spans[0].col, 2);
        assert_eq!(spans[0].cells.len(), 2);
    }

    #[test]
    fn apply_reconstructs_next() {
        let a = Grid::new(8, 2);
        let mut b = a.clone();
        b.put_str(1, 0, "hello", Style::new(), 8);
        b.put_str(0, 1, "界x", Style::new(), 8);
        let mut mirror = a.clone();
        apply(&mut mirror, &diff(&a, &b));
        assert_eq!(mirror, b);
        let mut fresh = Grid::new(8, 2);
        apply(&mut fresh, &full(&b));
        assert_eq!(fresh, b);
    }
}
