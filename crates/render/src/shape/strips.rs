//! Sparse strips: which parts of an outline's box the GPU passes touch.
//!
//! The winding integral of a pixel is the sum, over every edge crossing its row, of the
//! edge's height within the row times the fraction of the pixel right of the edge. Right
//! of every edge crossing a row, that sum stops changing along the row. So each box is cut
//! into bands of [`BAND`] rows; in a band, the columns some edge crosses are merged into
//! intervals, and between intervals the integral of each row is a constant: the heights
//! of every edge left of it, added up on the CPU (the carry). Each edge draws its
//! accumulate quad only to the end of its interval; the paint pass reads the
//! accumulation, plus the carry from the left, only in intervals, and paints the gaps
//! between them with the carry alone, skipping the ones it leaves empty. The GPU work
//! becomes about the outline's area once, plus its edges' strips, instead of about twice
//! the box's area.

use super::FillRule;

/// Rows per band. Bands start at canvas rows that are multiples of it, so a paint quad's
/// carries can be indexed by canvas row modulo it.
pub(super) const BAND: u32 = 4;

/// Paint piece kinds: coverage from the carry alone, or the carry plus the accumulation.
pub(super) const CARRY: u32 = 0;
pub(super) const ACCUMULATED: u32 = 1;

/// One quad of a pass over columns `rect[0]..rect[2]` and rows `rect[1]..rect[3]` of its
/// item's box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Piece {
    pub rect: [f32; 4],
    /// Accumulate: the edge's end points in box coordinates. Paint: the winding integral
    /// carried in from the left of the piece, per canvas row modulo [`BAND`].
    pub data: [f32; 4],
    pub item: u32,
    /// Paint: [`CARRY`] or [`ACCUMULATED`].
    pub kind: u32,
}

impl Piece {
    pub fn area(&self) -> u64 {
        ((self.rect[2] - self.rect[0]) * (self.rect[3] - self.rect[1])) as u64
    }
}

/// Coverage from a winding integral, as `shape.wgsl`'s `coverage`.
pub(super) fn coverage(a: f32, rule: FillRule) -> f32 {
    match rule {
        FillRule::EvenOdd => {
            let half = a * 0.5;
            1.0 - (1.0 - (half - half.floor()) * 2.0).abs()
        }
        FillRule::NonZero => a.abs().min(1.0),
    }
}

/// An edge's part in one band: the cells it covers partly, its rows, and its height in
/// each row.
struct Span {
    x0: f32,
    x1: f32,
    rows: (f32, f32),
    edge: usize,
    dy: [f32; 4],
}

/// The carry as stored: sums that land within 1e-5 of a whole number are snapped to it,
/// so a gap wound zero times (or twice, even-odd) is skipped.
fn snap(carry: f64) -> f32 {
    let whole = carry.round();
    if (carry - whole).abs() < 1e-5 {
        whole as f32
    } else {
        carry as f32
    }
}

/// Appends `piece`, or extends the piece of the band above that it continues (same
/// kind, columns and data, starting on the row that one ends). Pieces come left to right
/// in every band, so `cursor` walks the band above once.
fn push_merged(
    out: &mut Vec<Piece>,
    above: &[usize],
    cursor: &mut usize,
    current: &mut Vec<usize>,
    piece: Piece,
) {
    while *cursor < above.len() && out[above[*cursor]].rect[0] < piece.rect[0] {
        *cursor += 1;
    }
    for &i in &above[*cursor..] {
        let p = &mut out[i];
        if p.rect[0] != piece.rect[0] {
            break;
        }
        if p.rect[3] == piece.rect[1]
            && p.rect[2] == piece.rect[2]
            && p.kind == piece.kind
            && p.data == piece.data
        {
            p.rect[3] = piece.rect[3];
            current.push(i);
            return;
        }
    }
    current.push(out.len());
    out.push(piece);
}

/// Bins the `edges` (end points in box coordinates, none horizontal) of item `item`,
/// whose box is `rect` (canvas x, y, width, height), into accumulate pieces (`acc`, one
/// per edge and band, merged down the bands where they continue) and paint pieces
/// (`paint`, the intervals and the gaps that have coverage).
pub(super) fn bin(
    edges: &[[f32; 4]],
    rect: [u32; 4],
    rule: FillRule,
    item: u32,
    acc: &mut Vec<Piece>,
    paint: &mut Vec<Piece>,
) {
    let [_, by, w, h] = rect;
    let (wf, hf) = (w as f32, h as f32);
    // Edges by band (a counting sort): band b's edges are order[starts[b]..starts[b + 1]].
    let first_band = by / BAND;
    let bands = ((by + h).div_ceil(BAND) - first_band) as usize;
    let mut reach = Vec::with_capacity(edges.len());
    let mut starts = vec![0u32; bands + 1];
    for e in edges {
        let lo = e[1].min(e[3]).max(0.0);
        let hi = e[1].max(e[3]).min(hf);
        if hi <= lo {
            // Wholly above or below the box: no band.
            reach.push((1, 0));
            continue;
        }
        let first = ((by + lo.floor() as u32) / BAND - first_band) as usize;
        let last = ((by + hi.ceil() as u32 - 1) / BAND - first_band) as usize;
        reach.push((first, last));
        for count in &mut starts[first + 1..=last + 1] {
            *count += 1;
        }
    }
    for b in 0..bands {
        starts[b + 1] += starts[b];
    }
    let mut order = vec![0u32; starts[bands] as usize];
    let mut next = starts.clone();
    for (i, &(first, last)) in reach.iter().enumerate() {
        if first > last {
            continue;
        }
        for slot in &mut next[first..=last] {
            order[*slot as usize] = i as u32;
            *slot += 1;
        }
    }

    let mut spans: Vec<Span> = Vec::new();
    let (mut acc_above, mut acc_here) = (Vec::new(), Vec::new());
    let (mut paint_above, mut paint_here) = (Vec::new(), Vec::new());
    for b in 0..bands {
        let group = &order[starts[b] as usize..starts[b + 1] as usize];
        if group.is_empty() {
            continue;
        }
        let band = first_band + b as u32;
        let row0 = (band * BAND).max(by) - by;
        let row1 = ((band + 1) * BAND).min(by + h) - by;
        let (top, bottom) = (row0 as f32, row1 as f32);
        spans.clear();
        for &i in group {
            let [x0, y0, x1, y1] = edges[i as usize];
            let ya = y0.min(y1).max(top);
            let yb = y0.max(y1).min(bottom);
            if yb <= ya || y0 == y1 {
                continue;
            }
            // The edge's columns in this band, as the shader computes them per row.
            let k = (x1 - x0) / (y1 - y0);
            let xa = x0 + (ya - y0) * k;
            let xb = x0 + (yb - y0) * k;
            let start = xa.min(xb).floor().max(0.0);
            if start >= wf {
                // Right of the box: it changes no pixel in it.
                continue;
            }
            let end = xa.max(xb).ceil().min(wf).max(start + 1.0);
            let mut dy = [0.0; 4];
            for r in row0..row1 {
                let (r0, r1) = (r as f32, r as f32 + 1.0);
                dy[((by + r) % BAND) as usize] = y1.clamp(r0, r1) - y0.clamp(r0, r1);
            }
            spans.push(Span {
                x0: start,
                x1: end,
                rows: (ya.floor(), yb.ceil()),
                edge: i as usize,
                dy,
            });
        }
        // Columns are whole numbers from 0.
        spans.sort_unstable_by_key(|s| s.x0 as u32);

        let (mut acc_cursor, mut paint_cursor) = (0, 0);
        let mut carry = [0.0f64; 4];
        let mut left = 0.0f32;
        // A gap between intervals, if the carry gives any of its rows coverage.
        let gap = |from: f32, to: f32, carry: &[f64; 4]| {
            let data = carry.map(snap);
            let covered =
                (row0..row1).any(|r| coverage(data[((by + r) % BAND) as usize], rule) > 0.0);
            (from < to && covered).then_some(Piece {
                rect: [from, top, to, bottom],
                data,
                item,
                kind: CARRY,
            })
        };
        let mut s = 0;
        while s < spans.len() {
            let a = spans[s].x0;
            let mut b = spans[s].x1;
            let mut e = s + 1;
            while e < spans.len() && spans[e].x0 < b {
                b = b.max(spans[e].x1);
                e += 1;
            }
            if let Some(piece) = gap(left, a, &carry) {
                push_merged(
                    paint,
                    &paint_above,
                    &mut paint_cursor,
                    &mut paint_here,
                    piece,
                );
            }
            let interval = Piece {
                rect: [a, top, b, bottom],
                data: carry.map(snap),
                item,
                kind: ACCUMULATED,
            };
            push_merged(
                paint,
                &paint_above,
                &mut paint_cursor,
                &mut paint_here,
                interval,
            );
            for span in &spans[s..e] {
                let piece = Piece {
                    rect: [span.x0, span.rows.0, b, span.rows.1],
                    data: edges[span.edge],
                    item,
                    kind: 0,
                };
                push_merged(acc, &acc_above, &mut acc_cursor, &mut acc_here, piece);
                for (c, d) in carry.iter_mut().zip(span.dy) {
                    *c += f64::from(d);
                }
            }
            left = b;
            s = e;
        }
        if let Some(piece) = gap(left, wf, &carry) {
            push_merged(
                paint,
                &paint_above,
                &mut paint_cursor,
                &mut paint_here,
                piece,
            );
        }
        std::mem::swap(&mut acc_above, &mut acc_here);
        std::mem::swap(&mut paint_above, &mut paint_here);
        acc_here.clear();
        paint_here.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binned(edges: &[[f32; 4]], rect: [u32; 4], rule: FillRule) -> (Vec<Piece>, Vec<Piece>) {
        let (mut acc, mut paint) = (Vec::new(), Vec::new());
        bin(edges, rect, rule, 0, &mut acc, &mut paint);
        (acc, paint)
    }

    /// A rectangle's interior is one carry piece and each side one accumulate piece, all
    /// merged down the bands.
    #[test]
    fn a_rectangle_is_two_edge_strips_and_one_solid_piece() {
        // Box 2..21 x 3..20 on the canvas; the rectangle 0.5..18.25 x 0.25..16.5 in it.
        let edges = [[0.5, 16.5, 0.5, 0.25], [18.25, 0.25, 18.25, 16.5]];
        let (acc, paint) = binned(&edges, [2, 3, 19, 17], FillRule::NonZero);
        assert_eq!(acc.len(), 2, "{acc:?}");
        assert_eq!(acc[0].rect, [0.0, 0.0, 1.0, 17.0]);
        assert_eq!(acc[1].rect, [18.0, 0.0, 19.0, 17.0]);
        let solid: Vec<_> = paint.iter().filter(|p| p.kind == CARRY).collect();
        // The first band (canvas row 3) and the last (rows 16 to 19) hold partial rows.
        let full = solid
            .iter()
            .find(|p| p.data == [-1.0; 4])
            .expect("a fully covered piece");
        assert_eq!(
            full.rect,
            [1.0, 1.0, 18.0, 13.0],
            "rows 1 to 12, whole bands"
        );
        let area: u64 = paint.iter().map(Piece::area).sum();
        assert_eq!(
            area,
            19 * 17,
            "the box once, the empty rows and columns skipped"
        );
    }

    #[test]
    fn carries_follow_the_fill_rule() {
        // Two nested squares wound the same way: twice inside the inner one.
        let square = |a: f32, b: f32| [[a, b, a, a], [b, a, b, b]];
        let mut edges = square(0.0, 16.0).to_vec();
        edges.extend(square(4.0, 12.0));
        let (_, nonzero) = binned(&edges, [0, 0, 16, 16], FillRule::NonZero);
        let (_, evenodd) = binned(&edges, [0, 0, 16, 16], FillRule::EvenOdd);
        let carried =
            |p: &[Piece]| -> u64 { p.iter().filter(|p| p.kind == CARRY).map(Piece::area).sum() };
        let twice = |p: &Piece| p.kind == CARRY && p.data == [-2.0; 4];
        assert!(nonzero.iter().any(twice));
        assert!(
            !evenodd.iter().any(twice),
            "even-odd leaves the inner square empty"
        );
        assert_eq!(carried(&nonzero) - carried(&evenodd), 7 * 8);
    }

    #[test]
    fn edges_beyond_the_box_sides_carry_or_vanish() {
        // Left side off the canvas (x = -5), right side past the box (x = 40).
        let edges = [[-5.0, 8.0, -5.0, 0.0], [40.0, 0.0, 40.0, 8.0]];
        let (acc, paint) = binned(&edges, [0, 0, 30, 8], FillRule::NonZero);
        assert_eq!(acc.len(), 1, "the right side changes nothing in the box");
        assert_eq!(acc[0].rect, [0.0, 0.0, 1.0, 8.0]);
        let solid: Vec<_> = paint.iter().filter(|p| p.kind == CARRY).collect();
        assert_eq!(solid.len(), 1);
        assert_eq!(solid[0].rect, [1.0, 0.0, 30.0, 8.0]);
    }
}
