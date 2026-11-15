//! Rounded and square path strokes shared by grid frames and popup decorations.

use super::{Metrics, Rect};
use gpui::{Bounds, Path, PathBuilder, Pixels, Point, point, px};
use smallvec::SmallVec;
use std::{f32::consts::FRAC_PI_2, ops::Range};

/// Keep the solid line weight proportional to the grid font as before.
const SOLID_STROKE_EM: f32 = 0.10;
/// Dashed stroke weight in logical pixels.
const DASHED_STROKE_WIDTH: f32 = 1.2;
/// Nominal length of a dash and the gap after it in logical pixels.
const DASH_LENGTH: f32 = 9.;
const DASH_GAP: f32 = 5.;

/// Shape and stroke pattern of a grid outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) enum OutlineStyle {
    /// Rounded corners with a continuous stroke.
    Solid,
    /// Rounded corners with a broken stroke.
    Dashed,
    /// Square corners with a broken stroke.
    DashedSquare,
}

/// Columns left open on one horizontal edge, usually for a title or button.
pub(super) type EdgeGaps = SmallVec<[Range<usize>; 2]>;

/// An outline retained alongside a frame's text and icons.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Outline {
    /// Frame-local grid rectangle including the border cells.
    pub area: Rect,
    /// Theme role for the stroke.
    pub color: u32,
    /// Shape and stroke pattern.
    pub style: OutlineStyle,
    /// Frame-local columns skipped along the top edge.
    pub top: EdgeGaps,
    /// Frame-local columns skipped along the bottom edge.
    pub bottom: EdgeGaps,
}

impl Outline {
    /// Tessellate the stroke using `metrics` and the panel's `origin`, for paint or backdrop capture.
    pub fn path(&self, origin: Point<Pixels>, metrics: Metrics) -> Option<Path<Pixels>> {
        let metrics = Metrics { origin, ..metrics };
        let gaps = |gaps: &EdgeGaps| {
            gaps.iter()
                .map(|range| {
                    metrics.at(range.start as f32, 0.).x..metrics.at(range.end as f32, 0.).x
                })
                .collect::<SmallVec<[_; 2]>>()
        };
        outline_path(
            cell_center_bounds(self.area.pixels(metrics), metrics),
            self.style,
            metrics.font_size,
            &gaps(&self.top),
            &gaps(&self.bottom),
        )
    }
}

/// Align the stroke centerline with the first and last cells of `grid` using `metrics`.
pub(super) fn cell_center_bounds(grid: Bounds<Pixels>, metrics: Metrics) -> Bounds<Pixels> {
    let inset = point(px(metrics.cw * 0.5), px(metrics.lh * 0.5));
    Bounds::from_corners(grid.origin + inset, grid.bottom_right() - inset)
}

/// Build a `style` stroke along `bounds`, scaling the solid weight with `font_size`.
/// The `top` and `bottom` ranges are gaps in logical x coordinates.
pub(in crate::grid) fn outline_path(
    bounds: Bounds<Pixels>,
    style: OutlineStyle,
    font_size: f32,
    top: &[Range<Pixels>],
    bottom: &[Range<Pixels>],
) -> Option<Path<Pixels>> {
    if bounds.is_empty() {
        return None;
    }
    let radius = if style == OutlineStyle::DashedSquare {
        px(0.)
    } else {
        px(7.).min(bounds.size.width.min(bounds.size.height) * 0.25)
    };
    let left = bounds.left() + radius;
    let right = bounds.right() - radius;
    let top_gaps = edge_gaps(top, left..right);
    let bottom_gaps = edge_gaps(bottom, left..right);
    let closed = top_gaps.is_empty() && bottom_gaps.is_empty();
    let mut spans = vec![vec![point(left, bounds.top())]];
    split_edge(&mut spans, top_gaps, bounds.top());
    for (corner, center) in [
        point(right, bounds.top() + radius),
        point(right, bounds.bottom() - radius),
        point(left, bounds.bottom() - radius),
        point(left, bounds.top() + radius),
    ]
    .into_iter()
    .enumerate()
    {
        if corner == 2 {
            split_edge(
                &mut spans,
                bottom_gaps.iter().rev().map(|gap| gap.end..gap.start),
                bounds.bottom(),
            );
        }
        let span = spans.last_mut()?;
        if radius == px(0.) {
            span.push(center);
        } else {
            span.extend((0..=8).map(|step| {
                let angle = (corner as f32 - 1. + step as f32 / 8.) * FRAC_PI_2;
                center + point(radius * angle.cos(), radius * angle.sin())
            }));
        }
    }
    // Rejoin at the starting tangent, so only the requested gaps break the stroke.
    let first = spans.remove(0);
    if closed {
        spans.push(first);
    } else {
        spans.last_mut()?.extend(first.into_iter().skip(1));
    }
    let width = px(match style {
        OutlineStyle::Solid => font_size * SOLID_STROKE_EM,
        _ => DASHED_STROKE_WIDTH,
    });
    let mut outline = Path::new(bounds.origin);
    for mut span in spans {
        span.dedup();
        if closed && span.first() == span.last() {
            span.pop();
        }
        if span.len() < 2 {
            continue;
        }
        let mut builder = PathBuilder::stroke(width);
        if style != OutlineStyle::Solid {
            let length = span
                .windows(2)
                .map(|pair| segment_length(pair[0], pair[1]))
                .sum::<f32>()
                + if closed {
                    segment_length(span[0], span[span.len() - 1])
                } else {
                    0.
                };
            builder = builder.dash_array(&equal_dashes(length));
        }
        builder.add_polygon(&span, closed);
        outline.vertices.extend(builder.build().ok()?.vertices);
    }
    // GPUI uses the path bounds when clipping and scheduling the mesh.
    outline.bounds = bounds.dilate(width);
    (!outline.vertices.is_empty()).then_some(outline)
}

/// Clamp and merge `gaps` along the straight portion of an edge.
fn edge_gaps(gaps: &[Range<Pixels>], edge: Range<Pixels>) -> SmallVec<[Range<Pixels>; 2]> {
    let mut gaps: SmallVec<[_; 2]> = gaps
        .iter()
        .map(|gap| gap.start.max(edge.start)..gap.end.min(edge.end))
        .filter(|gap| !gap.is_empty())
        .collect();
    gaps.sort_unstable_by(|a, b| f32::from(a.start).total_cmp(&f32::from(b.start)));
    let mut merged: SmallVec<[Range<Pixels>; 2]> = SmallVec::new();
    for gap in gaps {
        if let Some(previous) = merged.last_mut()
            && gap.start <= previous.end
        {
            previous.end = previous.end.max(gap.end);
        } else {
            merged.push(gap);
        }
    }
    merged
}

/// Split the current stroke on a horizontal edge at the ordered `gaps` at height `y`.
fn split_edge(
    spans: &mut Vec<Vec<Point<Pixels>>>,
    gaps: impl IntoIterator<Item = Range<Pixels>>,
    y: Pixels,
) {
    for gap in gaps {
        spans
            .last_mut()
            .expect("initial outline span")
            .push(point(gap.start, y));
        spans.push(vec![point(gap.end, y)]);
    }
}

/// Measure the straight segment from `start` to `end` in logical pixels.
fn segment_length(start: Point<Pixels>, end: Point<Pixels>) -> f32 {
    let delta = end - start;
    f32::from(delta.x).hypot(delta.y.into())
}

/// Fit whole dash repeats along `length` and absorb rounding in the final off segment.
pub(super) fn equal_dashes(length: f32) -> Vec<Pixels> {
    let repeats = (length / (DASH_LENGTH + DASH_GAP)).round().max(1.) as usize;
    let period = length / repeats as f32;
    let dash = DASH_LENGTH.min(0.75 * period);
    let mut pattern = [px(dash), px(period - dash)].repeat(repeats);
    pattern[2 * repeats - 1] += px(1.);
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dashed paths fit complete strokes and a full gap around the selected length.
    #[test]
    fn dashes_fit_the_outline_without_a_short_last_stroke() {
        for length in [282.3, 1070.5, 1519.97] {
            let pattern = equal_dashes(length);
            assert!(
                pattern
                    .iter()
                    .step_by(2)
                    .all(|dash| *dash == px(DASH_LENGTH))
            );
            let last_dash_end: f32 = pattern[..pattern.len() - 1]
                .iter()
                .map(|&value| f32::from(value))
                .sum();
            assert!((length - last_dash_end - f32::from(pattern[1])).abs() < 0.01);
        }
    }

    /// The mesh leaves titles and footer buttons clear while the surrounding stroke stays visible.
    #[test]
    fn solid_outline_leaves_multiple_top_and_bottom_gaps() {
        let bounds = Bounds::from_corners(point(px(5.), px(10.)), point(px(105.), px(70.)));
        let path = outline_path(
            bounds,
            OutlineStyle::Solid,
            16.,
            &[px(20.)..px(50.), px(75.)..px(95.)],
            Some(px(30.)..px(60.)).as_slice(),
        )
        .unwrap();
        for (y, gaps, present) in [
            (px(10.), &[px(20.)..px(50.), px(75.)..px(95.)][..], px(60.)),
            (px(70.), Some(px(30.)..px(60.)).as_slice(), px(75.)),
        ] {
            let mut visible = false;
            for triangle in path.vertices.as_chunks::<3>().0 {
                let points = triangle.each_ref().map(|vertex| vertex.xy_position);
                if points.iter().all(|p| (p.y - y).abs() <= px(1.)) {
                    let left = points.iter().map(|p| p.x).reduce(Pixels::min).unwrap();
                    let right = points.iter().map(|p| p.x).reduce(Pixels::max).unwrap();
                    visible |= left <= present && present <= right;
                    assert!(gaps.iter().all(|gap| right <= gap.start || left >= gap.end));
                }
            }
            assert!(visible);
        }
    }
}
