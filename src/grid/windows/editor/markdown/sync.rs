//! Translate between source viewport rows and rendered Markdown pixels.

use super::PreviewScrollOrigin;
use nanobug::document::Document;

/// Correspondence between the scrollable parts of two views of one Markdown document.
pub(super) struct ScrollMap<'a> {
    /// Interior points ordered by physical source row and logical preview pixels.
    points: &'a [(f32, f32)],
    /// The active source document supplies the visible rows after folding.
    doc: &'a Document,
    /// Last full screen of source rows and last full screen of preview pixels.
    end: (f32, f32),
    /// Largest editor scroll position, in visible source rows.
    source_max: usize,
}

impl<'a> ScrollMap<'a> {
    /// Keep layout `points` reachable by both viewports and pair their natural scroll limits.
    pub(super) fn new(
        points: &'a [(f32, f32)],
        doc: &'a Document,
        source_height: usize,
        preview_max: usize,
    ) -> Option<Self> {
        let source_max = doc.display_rows().saturating_sub(source_height.max(1));
        if source_max == 0 || preview_max == 0 {
            return None;
        }
        let end = (doc.document_row(source_max) as f32, preview_max as f32);
        let first = points.partition_point(|&(row, pixel)| row <= 0. || pixel <= 0.);
        let bounded = &points[first..];
        let last = bounded.partition_point(|&(row, pixel)| row < end.0 && pixel < end.1);
        Some(Self {
            points: &bounded[..last],
            doc,
            end,
            source_max,
        })
    }

    /// Convert the animated source `display_row` to a preview offset in logical pixels.
    pub(super) fn preview_top(&self, display_row: f32) -> f32 {
        let row = display_row.clamp(0., self.source_max as f32);
        let first = self.doc.document_row(row.floor() as usize) as f32;
        let next = self.doc.document_row(row.ceil() as usize) as f32;
        self.map(first + (next - first) * row.fract(), |p| p)
    }

    /// Convert the animated preview offset `pixels` to a visible editor row, accounting for folds.
    fn source_top(&self, pixels: f32) -> f32 {
        let row = self.map(pixels, |(row, pixel)| (pixel, row));
        let first = self.doc.display_row(row.floor() as usize) as f32;
        let next = self.doc.display_row(row.ceil() as usize) as f32;
        first + (next - first) * row.fract()
    }

    /// Follow preview pixels `current` from the displayed `origin` to `target`.
    /// Resolve the whole source row up front so it is reached by the same animation.
    pub(super) fn follow_preview(
        &self,
        origin: PreviewScrollOrigin,
        current: f32,
        target: f32,
    ) -> (f32, usize) {
        let target_row = self.source_top(target);
        let end = target_row.round();
        if current == target || origin.preview == target {
            return (end, self.source_max);
        }
        let start = self.source_top(origin.preview);
        let distance = target_row - start;
        // A folded interval can map every preview position to the same visible row.
        let progress = if distance.abs() > f32::EPSILON {
            (self.source_top(current) - start) / distance
        } else {
            (current - origin.preview) / (target - origin.preview)
        };
        let position = origin.source + (end - origin.source) * progress.clamp(0., 1.);
        (position, self.source_max)
    }

    /// Interpolate `at` along the input axis chosen by `axes`, including both scroll endpoints.
    fn map(&self, at: f32, axes: impl Fn((f32, f32)) -> (f32, f32)) -> f32 {
        let end = axes(self.end);
        let at = at.clamp(0., end.0);
        let next = self.points.partition_point(|&point| axes(point).0 <= at);
        let left = next
            .checked_sub(1)
            .map_or((0., 0.), |index| axes(self.points[index]));
        let right = self.points.get(next).copied().map_or(end, &axes);
        left.1 + (right.1 - left.1) * ((at - left.0) / (right.0 - left.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layout anchors, rather than document percentages, drive both directions and meet at the end.
    #[test]
    fn markdown_sync_maps_content_and_both_scroll_limits() {
        let doc = Document::new(&"row\n".repeat(100));
        let points = [
            (0., 24.),
            (30., 200.),
            (40., 1000.),
            (60., 1400.),
            (95., 3000.),
        ];
        let map = ScrollMap::new(&points, &doc, 20, 2000).unwrap();
        assert_eq!(map.preview_top(0.), 0.);
        assert_eq!(map.preview_top(35.), 600.);
        assert_eq!(map.source_top(600.), 35.);
        assert_eq!(map.preview_top(81.), 2000.);
        assert_eq!(map.source_top(2000.), 81.);
        assert!(ScrollMap::new(&points, &doc, 110, 2000).is_none());
    }

    /// A preview position inside a folded region still corresponds to the fold's visible header.
    #[test]
    fn markdown_sync_resolves_folded_rows_in_the_source_view() {
        let mut doc = Document::new(&"row\n".repeat(20));
        doc.collapse_rows(3..8);
        let points = [(2., 150.), (5., 400.), (8., 700.), (12., 900.)];
        let map = ScrollMap::new(&points, &doc, 4, 1000).unwrap();
        assert_eq!(map.preview_top(3.), 700.);
        assert_eq!(map.source_top(400.), 2.);
        assert_eq!(map.source_top(700.), 3.);
    }

    /// Apply alignment throughout the existing animation and keep the preview's exact target.
    #[test]
    fn markdown_preview_settles_on_one_source_row_without_a_final_jump() {
        let doc = Document::new(&"row\n".repeat(30));
        let points = [(10., 300.), (20., 600.)];
        let map = ScrollMap::new(&points, &doc, 5, 800).unwrap();
        let origin = PreviewScrollOrigin {
            preview: 300.,
            source: 10.,
        };
        let target = 345.;
        assert_eq!(map.source_top(target), 11.5);
        assert_eq!(map.follow_preview(origin, 299., target).0, 10.);
        let near = map.follow_preview(origin, 344.9, target).0;
        let end = map.follow_preview(origin, target, target).0;
        assert_eq!(end, 12.);
        assert!((end - near).abs() < 0.01);

        let reversed = PreviewScrollOrigin {
            preview: 332.,
            source: 11.4,
        };
        assert_eq!(map.follow_preview(reversed, 333., 310.).0, reversed.source);
        assert_eq!(map.follow_preview(reversed, 310., 310.).0, 10.);
    }
}
