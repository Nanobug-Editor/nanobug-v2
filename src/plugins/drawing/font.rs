//! Shape plugin-owned OpenType fonts without registering them in the editor's font collection.

use anyhow::{Context, Result, ensure};
use gpui::{FillOptions, Path, PathBuilder, PathStyle, Pixels, point, px};
use nanobug_plugin_sdk::canvas::Point;
use rustybuzz::{
    Direction, Face, GlyphBuffer, UnicodeBuffer, shape,
    ttf_parser::{GlyphId, OutlineBuilder},
};
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation;

/// Bound the amount of outline data handed to the tessellator for one font draw command.
const MAX_OUTLINE_COMMANDS: usize = 12_000;

/// A shaped line in which rendering and caret measurements use the same advances and clusters.
pub(super) struct FontLine<'a> {
    face: Face<'a>,
    glyphs: GlyphBuffer,
    scale: f32,
    rtl: bool,
}

impl<'a> FontLine<'a> {
    /// Shape one line of text from the specified face of a verified font.
    pub(super) fn shape(bytes: &'a [u8], face_index: u32, text: &str, size: f32) -> Result<Self> {
        let face = Face::from_slice(bytes, face_index).context("Invalid plugin font face")?;
        let tables = face.tables();
        ensure!(
            tables.glyf.is_some() || tables.cff.is_some() || tables.cff2.is_some(),
            "Plugin font requires OpenType glyph outlines"
        );
        let scale = size / f32::from(face.as_ref().units_per_em());
        let mut text_buffer = UnicodeBuffer::new();
        text_buffer.push_str(text);
        text_buffer.guess_segment_properties();
        let rtl = text_buffer.direction() == Direction::RightToLeft;
        let glyphs = shape(&face, &[], text_buffer);
        ensure!(
            glyphs.len() <= 1024,
            "Plugin font has too many shaped glyphs"
        );
        Ok(Self {
            face,
            glyphs,
            scale,
            rtl,
        })
    }

    /// Tessellate the shaped glyph outlines in logical pixels relative to the requested baseline.
    pub(super) fn outline(&self, baseline: Point) -> Result<Path<Pixels>> {
        let mut path = PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::non_zero()));
        let mut outline = GlyphOutline {
            path: &mut path,
            baseline,
            scale: self.scale,
            commands: 0,
        };
        let mut pen = baseline;
        for (glyph, position) in self
            .glyphs
            .glyph_infos()
            .iter()
            .zip(self.glyphs.glyph_positions())
        {
            outline.baseline = [
                pen[0] + position.x_offset as f32 * self.scale,
                pen[1] - position.y_offset as f32 * self.scale,
            ];
            self.face
                .outline_glyph(GlyphId(glyph.glyph_id as u16), &mut outline);
            ensure!(
                outline.commands <= MAX_OUTLINE_COMMANDS,
                "Plugin font outline exceeds the drawing budget"
            );
            pen[0] += position.x_advance as f32 * self.scale;
            pen[1] -= position.y_advance as f32 * self.scale;
        }
        path.build()
    }

    /// Report the width, vertical metrics and grapheme positions of this exact shaped line.
    pub(super) fn measure(&self, text: &str) -> serde_json::Value {
        let mut advance = 0.;
        let mut clusters = BTreeMap::<usize, (f32, f32)>::new();
        for (glyph, position) in self
            .glyphs
            .glyph_infos()
            .iter()
            .zip(self.glyphs.glyph_positions())
        {
            let next = advance + position.x_advance as f32 * self.scale;
            clusters
                .entry(glyph.cluster as usize)
                .and_modify(|bounds| bounds.1 = next)
                .or_insert((advance, next));
            advance = next;
        }
        let positions = text.grapheme_indices(true)
            .map(|(offset, _)| {
                let x = clusters.range(..=offset).next_back().map_or(0., |(_, (left, right))| {
                    if self.rtl { *right } else { *left }
                });
                serde_json::json!({ "offset": offset, "x": x })
            })
            .chain([serde_json::json!({ "offset": text.len(), "x": if self.rtl { 0. } else { advance } })])
            .collect::<Vec<_>>();
        serde_json::json!({
            "width": advance,
            "ascent": f32::from(self.face.ascender()) * self.scale,
            "descent": -f32::from(self.face.descender()) * self.scale,
            "positions": positions,
        })
    }
}

/// Convert the font's y-up design coordinates to the Canvas's y-down coordinates.
struct GlyphOutline<'a> {
    path: &'a mut PathBuilder,
    baseline: Point,
    scale: f32,
    commands: usize,
}

impl GlyphOutline<'_> {
    fn position(&self, x: f32, y: f32) -> gpui::Point<Pixels> {
        point(
            px(self.baseline[0] + x * self.scale),
            px(self.baseline[1] - y * self.scale),
        )
    }

    fn within_budget(&mut self) -> bool {
        self.commands += 1;
        self.commands <= MAX_OUTLINE_COMMANDS
    }
}

impl OutlineBuilder for GlyphOutline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.within_budget() {
            self.path.move_to(self.position(x, y));
        }
    }

    fn line_to(&mut self, x: f32, y: f32) {
        if self.within_budget() {
            self.path.line_to(self.position(x, y));
        }
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        if self.within_budget() {
            self.path
                .curve_to(self.position(x, y), self.position(x1, y1));
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        if self.within_budget() {
            self.path.cubic_bezier_to(
                self.position(x, y),
                self.position(x1, y1),
                self.position(x2, y2),
            );
        }
    }

    fn close(&mut self) {
        if self.within_budget() {
            self.path.close();
        }
    }
}
