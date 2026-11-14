//! Shape a semantic Canvas field once for painting, hit testing and the shared caret.

use super::{SurfaceWindow, canvas::native_font};
use crate::{
    grid::render::Metrics,
    ui::{ime::Composition, theme::AppearanceSettings},
};
use crate::{
    grid::render::{Cursor, Rect, sample_waves},
    ui::theme::{ACCENT, SELECTION, Style, TEXT},
};
use gpui::{
    AnyElement, Bounds, ContentMask, Entity, Hsla, IntoElement, Pixels, Point, ShapedLine, Styled,
    TextAlign, TextRun, UnderlineStyle, Window, canvas, fill, point, px, rgba, size,
};
use nanobug::animation::cursor::Vec2;
use nanobug::plugins::session::Surface;
use nanobug_plugin_sdk::canvas::TextInput;
use std::{ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

pub(in crate::grid) struct NativeField {
    revision: u64,
    scale: f32,
    focused: bool,
    pub(in crate::grid) clip: Bounds<Pixels>,
    area: Bounds<Pixels>,
    line: ShapedLine,
    carets: Rc<[(usize, Pixels)]>,
    scroll: Pixels,
    caret: usize,
    selection: Range<usize>,
    preedit: Range<usize>,
    color: Hsla,
}

impl NativeField {
    /// Approximate host field and caret storage, excluding GPUI's shared shaped glyph cache.
    pub(in crate::grid) fn owned_layout_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of_val(self.carets.as_ref())
    }

    #[cfg(all(test, windows))]
    pub(in crate::grid) fn font_at_start(&self) -> Option<gpui::FontId> {
        self.line.font_id_for_index(0)
    }

    /// Map to a full grapheme boundary using exactly the native positions we paint.
    pub(in crate::grid) fn byte_at(&self, point: Point<Pixels>) -> usize {
        let x = point.x - self.area.origin.x + self.scroll;
        self.carets
            .iter()
            .min_by(|(_, a), (_, b)| f32::from(*a - x).abs().total_cmp(&f32::from(*b - x).abs()))
            .map_or(0, |(byte, _)| *byte)
    }

    pub(in crate::grid) fn caret_bounds(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(
                (self.area.origin.x + self.x(self.caret) - self.scroll).clamp(
                    self.clip.left(),
                    (self.clip.right() - px(2.)).max(self.clip.left()),
                ),
                self.clip.top(),
            ),
            size(px(2.).min(self.clip.size.width), self.clip.size.height),
        )
    }

    fn x(&self, byte: usize) -> Pixels {
        let position = self.carets.partition_point(|(offset, _)| *offset < byte);
        self.carets
            .get(position)
            .map_or(self.line.width(), |(_, x)| *x)
    }

    fn range_bounds(&self, origin: Point<Pixels>, range: &Range<usize>) -> Bounds<Pixels> {
        let a = self.x(range.start);
        let b = self.x(range.end);
        Bounds::new(
            origin + point(a.min(b), px(0.)),
            size((a - b).abs(), self.area.size.height),
        )
    }
}

impl SurfaceWindow {
    pub(in crate::grid) fn current_field(
        &self,
        surface: &Surface,
        panel: Rect,
        metrics: Metrics,
    ) -> Option<&Rc<NativeField>> {
        let field = self.native_field.as_ref()?;
        (field.revision == surface.revision && panel.pixels(metrics).contains(&field.clip.origin))
            .then_some(field)
    }
    pub(in crate::grid) fn build_layout(
        &self,
        panel: Rect,
        metrics: Metrics,
        surface: &Surface,
        composition: &Composition,
        appearance: AppearanceSettings,
        window: &Window,
    ) -> Option<Rc<NativeField>> {
        let panel = panel.pixels(metrics);
        let TextInput {
            bounds: [x, y, width, height],
            font,
            font_options,
            size: font_size,
            color,
        } = surface.frame.canvas.as_ref()?.text_input.as_ref()?;
        let area = Bounds::new(
            panel.origin + point(px(*x), px(*y)),
            size(px(*width), px(*height)),
        );
        let clip = area.intersect(&panel);
        if clip.is_empty() {
            return None;
        }
        let draft = &self.draft;
        let (text, caret) = self.field.displayed(draft, composition);
        let selection = self.field.selection(draft);
        let (selection, preedit) = if composition.is_empty() {
            (selection, 0..0)
        } else {
            let start = selection.start;
            (0..0, start..start + composition.text.len())
        };
        let previous = self.native_field.as_ref().filter(|field| {
            field.revision == surface.revision && field.scale == window.scale_factor()
        });
        if let Some(previous) = previous.filter(|field| {
            field.area == area
                && field.clip == clip
                && field.focused == self.focused
                && field.line.text.as_ref() == text.as_ref()
                && field.caret == caret
                && field.selection == selection
                && field.preedit == preedit
        }) {
            return Some(Rc::clone(previous));
        }
        let text: gpui::SharedString = if text.contains(['\n', '\r']) {
            text.replace(['\n', '\r'], " ").into()
        } else {
            text.into_owned().into()
        };
        let style = appearance.style(window.appearance());
        let color = rgba(color.unwrap_or((style.foreground(TEXT) << 8) | 255)).into();
        let (line, carets) =
            if let Some(previous) = previous.filter(|previous| previous.line.text == text) {
                (previous.line.clone(), previous.carets.clone())
            } else {
                let line = window.text_system().shape_line(
                    text.clone(),
                    px(*font_size),
                    &[TextRun {
                        len: text.len(),
                        font: native_font(font.as_deref(), font_options.as_ref(), style.font),
                        color,
                        ..TextRun::default()
                    }],
                    None,
                );
                let mut glyphs = line.runs.iter().flat_map(|run| &run.glyphs).peekable();
                let carets = text
                    .grapheme_indices(true)
                    .map(|(byte, _)| byte)
                    .chain([text.len()])
                    .map(|byte| {
                        while glyphs.next_if(|glyph| glyph.index < byte).is_some() {}
                        (
                            byte,
                            glyphs.peek().map_or(line.width(), |glyph| glyph.position.x),
                        )
                    })
                    .collect::<Vec<_>>();
                (line, carets.into())
            };
        let index = carets.partition_point(|(byte, _)| *byte < caret);
        let x = carets.get(index).map_or(line.width(), |(_, x)| *x);
        let mut scroll = previous.map_or(px(0.), |field| field.scroll);
        if self.focused {
            let left = clip.left() - area.left();
            let right = (clip.right() - area.left() - px(2.)).max(left);
            scroll = scroll.max(x - right).min(x - left).max(px(0.));
        }
        Some(Rc::new(NativeField {
            revision: surface.revision,
            scale: window.scale_factor(),
            focused: self.focused,
            area,
            clip,
            line,
            carets,
            scroll,
            caret,
            selection,
            preedit,
            color,
        }))
    }
}

pub(in crate::grid) fn element(
    field: Rc<NativeField>,
    panel: Point<Pixels>,
    cursor: Option<Entity<Cursor>>,
    style: Style,
) -> AnyElement {
    let clip = field.clip;
    canvas(
        |_, _, _| (),
        move |_, (), window, cx| {
            let origin = field.area.origin - point(field.scroll, px(0.));
            let height = field.area.size.height;
            window.with_content_mask(Some(ContentMask { bounds: field.clip }), |window| {
                if cursor.is_some() && !field.selection.is_empty() {
                    window.paint_quad(fill(
                        field.range_bounds(origin, &field.selection),
                        rgba(style.tint(SELECTION)),
                    ));
                }
                let waves = cursor
                    .as_ref()
                    .map_or_else(Vec::new, |cursor| cursor.read(cx).nearby_text_waves());
                if waves.is_empty() {
                    let _ = field
                        .line
                        .paint(origin, height, TextAlign::Left, None, window, cx);
                } else {
                    let baseline =
                        origin.y + (height + field.line.ascent - field.line.descent) / 2.;
                    for run in &field.line.runs {
                        for glyph in &run.glyphs {
                            let at = point(origin.x, baseline) + glyph.position;
                            let (light, shift) = sample_waves(
                                &waves,
                                Vec2::new(f32::from(at.x), f32::from(origin.y + height / 2.)),
                            );
                            if glyph.is_emoji {
                                let _ = window.paint_emoji(
                                    at + shift,
                                    run.font_id,
                                    glyph.id,
                                    field.line.font_size,
                                );
                            } else {
                                let tint = rgba(
                                    (style.foreground(ACCENT) << 8)
                                        | (light.clamp(0., 1.) * 205.) as u32,
                                );
                                let ink = field.color.blend(tint.into());
                                let _ = window.paint_glyph(
                                    at + shift,
                                    run.font_id,
                                    glyph.id,
                                    field.line.font_size,
                                    ink,
                                );
                            }
                        }
                    }
                }
                if cursor.is_some() && !field.preedit.is_empty() {
                    let bounds = field.range_bounds(origin, &field.preedit);
                    window.paint_underline(
                        point(bounds.origin.x, bounds.bottom() - px(2.)),
                        bounds.size.width,
                        &UnderlineStyle {
                            color: Some(field.color),
                            thickness: px(1.),
                            wavy: false,
                        },
                    );
                }
            });
        },
    )
    .absolute()
    .left(clip.left() - panel.x)
    .top(clip.top() - panel.y)
    .w(clip.size.width)
    .h(clip.size.height)
    .into_any_element()
}
