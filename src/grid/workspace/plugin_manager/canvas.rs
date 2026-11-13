//! Map a validated plugin scene to native Canvas paint operations.

#[cfg(all(test, any(windows, target_os = "linux", target_os = "macos")))]
mod parity;

use super::interaction::UnderlineReport;
use crate::ui::theme::{GridFont, TEXT, style};
use gpui::{
    App, Background, BorderStyle, Bounds, BoxShadow, ColorSpace, ContentMask, Corners, CursorStyle,
    Edges, Hitbox, HitboxBehavior, Hsla, Interactivity, PaintQuad, Pixels, Point, ScaledPixels,
    StrikethroughStyle, TextAlign, TextRun, TransformationMatrix, UnderlineStyle, Window, canvas,
    checkerboard, font, linear_color_stop, linear_gradient, pattern_slash, point, px, radians,
    rgba, size, solid_background,
};
use nanobug::plugins::{
    drawing::{Asset, PreparedCanvas},
    session::Surface,
};
use nanobug_plugin_sdk::canvas::{
    Decoration, Draw, Fill, FontOptions, FontSlant, LineUnderlines, Rect, Rgba, TextAlignment,
    UnderlineGeometry,
};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// Measure a line with GPUI's font fallback and return positions at grapheme boundaries.
pub(super) fn measure(
    window: &Window,
    request: &nanobug_plugin_sdk::canvas::MeasureText,
    grid: GridFont,
) -> serde_json::Value {
    let runs = [TextRun {
        len: request.text.len(),
        font: native_font(request.font.as_deref(), request.font_options.as_ref(), grid),
        ..TextRun::default()
    }];
    let line =
        window
            .text_system()
            .shape_line(request.text.clone().into(), px(request.size), &runs, None);
    let positions = request.text.grapheme_indices(true).map(|(offset, _)| offset)
        .chain([request.text.len()]).map(|offset| {
            serde_json::json!({ "offset": offset, "x": f32::from(line.x_for_index(offset)) })
        }).collect::<Vec<_>>();
    serde_json::json!({ "width": f32::from(line.width()), "ascent": f32::from(line.ascent),
        "descent": f32::from(line.descent), "positions": positions })
}

/// Shape and measure with the same family, fallback and face refinements.
pub(super) fn native_font(
    family: Option<&str>,
    options: Option<&FontOptions>,
    grid: GridFont,
) -> gpui::Font {
    let mut native = family.map_or_else(|| grid.descriptor(), |family| font(family.to_owned()));
    if let Some(options) = options {
        if let Some(weight) = options.weight {
            native.weight = gpui::FontWeight(weight);
        }
        if let Some(slant) = options.slant {
            native.style = match slant {
                FontSlant::Normal => gpui::FontStyle::Normal,
                FontSlant::Italic => gpui::FontStyle::Italic,
                FontSlant::Oblique => gpui::FontStyle::Oblique,
            };
        }
        if let Some(features) = &options.features {
            native.features = gpui::FontFeatures(Arc::new(
                features
                    .iter()
                    .map(|(tag, value)| (tag.clone(), *value))
                    .collect(),
            ));
        }
        if let Some(fallbacks) = &options.fallbacks {
            native.fallbacks = Some(gpui::FontFallbacks::from_fonts(fallbacks.clone()));
        }
    }
    native
}

/// Paint the same immutable scene that supplies the host's hit targets.
pub(super) fn element(
    surface: &Surface,
    input: Option<Bounds<Pixels>>,
    underline_report: Option<UnderlineReport>,
) -> Option<gpui::Canvas<Vec<(Hitbox, CursorStyle)>>> {
    element_with_underlines(
        surface,
        input,
        underline_report.map(|report| {
            move |lines, window: &Window, cx: &mut App| report.schedule(lines, window, cx)
        }),
    )
}

/// Capture from the first actual paint only. Ordinary drawing never builds a geometry buffer.
fn element_with_underlines(
    surface: &Surface,
    input: Option<Bounds<Pixels>>,
    report: Option<impl FnOnce(Vec<LineUnderlines>, &Window, &mut App) + 'static>,
) -> Option<gpui::Canvas<Vec<(Hitbox, CursorStyle)>>> {
    let scene = Arc::clone(&surface.frame);
    let hit_scene = Arc::clone(&scene);
    let prepared = Arc::clone(surface.drawing.as_ref()?);
    Some(canvas(
        move |bounds, window, _| {
            let mut hitboxes = Vec::new();
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if let Some(canvas) = &hit_scene.canvas {
                    for region in &canvas.interaction.pointer {
                        let Some(cursor) = region.cursor else {
                            continue;
                        };
                        let [x, y, width, height] = region.bounds;
                        let hit =
                            Bounds::new(bounds.origin + p([x, y]), size(px(width), px(height)));
                        hitboxes.push((
                            window.insert_hitbox(hit, HitboxBehavior::Normal),
                            cursor_style(cursor),
                        ));
                    }
                }
                if !hitboxes.is_empty()
                    && let Some(input) = input
                {
                    hitboxes.push((
                        window.insert_hitbox(input, HitboxBehavior::Normal),
                        CursorStyle::IBeam,
                    ));
                }
            });
            hitboxes
        },
        move |bounds, hitboxes, window, cx| {
            let Some(canvas) = &scene.canvas else {
                return;
            };
            for (hitbox, style) in hitboxes {
                window.set_cursor_style(style, &hitbox);
            }
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let mut painter = Painter {
                    resources: &prepared,
                    path: 0,
                    origin: bounds.origin,
                    canvas_origin: bounds.origin,
                    underlines: report.as_ref().map(|_| Vec::new()),
                };
                painter.paint(&canvas.draw, window, cx);
                if let (Some(report), Some(lines)) = (report, painter.underlines) {
                    report(lines, window, cx);
                }
            });
        },
    ))
}

/// Match pinned GPUI cursor styles without exposing its native types to the guest.
fn cursor_style(style: nanobug_plugin_sdk::canvas::CursorStyle) -> CursorStyle {
    use nanobug_plugin_sdk::canvas::CursorStyle as Sdk;
    match style {
        Sdk::Arrow => CursorStyle::Arrow,
        Sdk::IBeam => CursorStyle::IBeam,
        Sdk::Crosshair => CursorStyle::Crosshair,
        Sdk::ClosedHand => CursorStyle::ClosedHand,
        Sdk::OpenHand => CursorStyle::OpenHand,
        Sdk::PointingHand => CursorStyle::PointingHand,
        Sdk::ResizeLeft => CursorStyle::ResizeLeft,
        Sdk::ResizeRight => CursorStyle::ResizeRight,
        Sdk::ResizeLeftRight => CursorStyle::ResizeLeftRight,
        Sdk::ResizeUp => CursorStyle::ResizeUp,
        Sdk::ResizeDown => CursorStyle::ResizeDown,
        Sdk::ResizeUpDown => CursorStyle::ResizeUpDown,
        Sdk::ResizeUpLeftDownRight => CursorStyle::ResizeUpLeftDownRight,
        Sdk::ResizeUpRightDownLeft => CursorStyle::ResizeUpRightDownLeft,
        Sdk::ResizeColumn => CursorStyle::ResizeColumn,
        Sdk::ResizeRow => CursorStyle::ResizeRow,
        Sdk::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        Sdk::OperationNotAllowed => CursorStyle::OperationNotAllowed,
        Sdk::DragLink => CursorStyle::DragLink,
        Sdk::DragCopy => CursorStyle::DragCopy,
        Sdk::ContextualMenu => CursorStyle::ContextualMenu,
    }
}

struct Painter<'a> {
    resources: &'a PreparedCanvas,
    path: usize,
    origin: Point<Pixels>,
    canvas_origin: Point<Pixels>,
    underlines: Option<Vec<LineUnderlines>>,
}

impl Painter<'_> {
    fn at(&self, value: [f32; 2]) -> Point<Pixels> {
        self.origin + p(value)
    }
    fn rect(&self, [x, y, width, height]: Rect) -> Bounds<Pixels> {
        Bounds::new(self.at([x, y]), size(px(width), px(height)))
    }
    fn ink(&self, color: Rgba) -> Hsla {
        Hsla::from(rgba(color))
    }

    /// Paint the next tessellated outline at this group's logical origin.
    fn paint_path(&mut self, fill: Background, window: &mut Window) {
        let mut path = self.resources.paths[self.path].clone();
        self.path += 1;
        path.bounds.origin += self.origin;
        for vertex in &mut path.vertices {
            vertex.xy_position += self.origin;
        }
        window.paint_path(path, fill);
    }

    fn fill(&self, fill: &Fill) -> Background {
        match fill {
            Fill::Solid { color } => solid_background(self.ink(*color)),
            Fill::Linear {
                angle,
                from,
                to,
                oklab,
            } => linear_gradient(
                *angle,
                linear_color_stop(self.ink(from.0), from.1),
                linear_color_stop(self.ink(to.0), to.1),
            )
            .color_space(if *oklab {
                ColorSpace::Oklab
            } else {
                ColorSpace::Srgb
            }),
            Fill::Slash {
                color,
                width,
                interval,
            } => pattern_slash(self.ink(*color), *width, *interval),
            Fill::Checkerboard { color, size } => checkerboard(self.ink(*color), *size),
        }
    }

    fn underline(&self, style: &Decoration) -> UnderlineStyle {
        UnderlineStyle {
            thickness: px(style.thickness),
            color: style.color.map(|ink| self.ink(ink)),
            wavy: style.wavy,
        }
    }

    fn strike(&self, style: &Decoration) -> StrikethroughStyle {
        StrikethroughStyle {
            thickness: px(style.thickness),
            color: style.color.map(|ink| self.ink(ink)),
        }
    }

    /// Apply the same font, color, and decoration rules to wrapped and unwrapped text.
    fn text_runs(
        &self,
        text: &str,
        runs: &[nanobug_plugin_sdk::canvas::TextRun],
        cx: &App,
    ) -> Vec<TextRun> {
        let style = style(cx);
        if runs.is_empty() {
            return vec![TextRun {
                len: text.len(),
                font: style.font.descriptor(),
                color: self.ink((style.foreground(TEXT) << 8) | 255),
                ..TextRun::default()
            }];
        }
        runs.iter()
            .map(|run| TextRun {
                len: run.length,
                font: native_font(run.font.as_deref(), run.font_options.as_ref(), style.font),
                color: self.ink(run.color),
                background_color: run.background.map(|ink| self.ink(ink)),
                underline: run.underline.as_ref().map(|style| self.underline(style)),
                strikethrough: run.strike.as_ref().map(|style| self.strike(style)),
            })
            .collect()
    }

    /// Preserve the native wave phase and convert the guest's local exclusions to device space.
    fn paint_underline(
        origin: Point<Pixels>,
        width: Pixels,
        style: &UnderlineStyle,
        exclusions: &[[f32; 2]],
        window: &mut Window,
    ) {
        if exclusions.is_empty() {
            window.paint_underline(origin, width, style);
        } else {
            let scale = window.scale_factor();
            let offsets = exclusions
                .iter()
                .map(|[left, right]| {
                    (origin.x + px(*left)).scale(scale)..(origin.x + px(*right)).scale(scale)
                })
                .collect::<smallvec::SmallVec<[_; 4]>>();
            window.paint_underline_with_exclusions(origin, width, style, &offsets);
        }
    }

    fn paint(&mut self, draw: &[Draw], window: &mut Window, cx: &mut App) {
        for command in draw {
            match command {
                Draw::Quad {
                    bounds,
                    fill,
                    corners: radii,
                    borders: [top, right, bottom, left],
                    border,
                    dashed,
                } => {
                    window.paint_quad(PaintQuad {
                        bounds: self.rect(*bounds),
                        corner_radii: corners(*radii),
                        background: self.fill(fill),
                        border_widths: Edges {
                            top: px(*top),
                            right: px(*right),
                            bottom: px(*bottom),
                            left: px(*left),
                        },
                        border_color: self.ink(*border),
                        border_style: if *dashed {
                            BorderStyle::Dashed
                        } else {
                            BorderStyle::Solid
                        },
                    });
                }
                Draw::Path { fill, .. } => {
                    self.paint_path(self.fill(fill), window);
                }
                Draw::PackageText { color, .. } => {
                    self.paint_path(solid_background(self.ink(*color)), window);
                }
                Draw::Shadows {
                    bounds,
                    corners: radii,
                    values,
                    inset,
                } => {
                    let shadows = values
                        .iter()
                        .map(|shadow| BoxShadow {
                            color: self.ink(shadow.color),
                            offset: p(shadow.offset),
                            blur_radius: px(shadow.blur),
                            spread_radius: px(shadow.spread),
                            inset: shadow.inset,
                        })
                        .collect::<Vec<_>>();
                    if *inset {
                        window.paint_inset_shadows(self.rect(*bounds), corners(*radii), &shadows);
                    } else {
                        window.paint_drop_shadows(self.rect(*bounds), corners(*radii), &shadows);
                    }
                }
                Draw::Text {
                    origin,
                    text,
                    size: font_size,
                    line_height,
                    wrap,
                    runs,
                    layout,
                } => {
                    let runs = self.text_runs(text, runs, cx);
                    let layout = layout.unwrap_or_default();
                    let align = text_align(layout.align);
                    let height = px(*line_height);
                    let mut at = self.at(*origin);
                    if let Ok(lines) = window.text_system().shape_text(
                        text.clone().into(),
                        px(*font_size),
                        &runs,
                        wrap.map(px),
                        Some(usize::from(layout.max_lines.unwrap_or(64))),
                    ) {
                        for line in lines {
                            let bounds = layout
                                .width
                                .map(|width| Bounds::new(at, size(px(width), height)));
                            let _ = line.paint_background(at, height, align, bounds, window, cx);
                            let _ = line.paint(at, height, align, bounds, window, cx);
                            at.y += height * (line.wrap_boundaries().len() + 1) as f32;
                        }
                    }
                }
                Draw::ShapedLine {
                    origin,
                    text,
                    size,
                    line_height,
                    runs,
                    layout,
                    underlines,
                    underline_tag,
                } => {
                    let runs = self.text_runs(text, runs, cx);
                    let line = window.text_system().shape_line(
                        text.clone().into(),
                        px(*size),
                        &runs,
                        None,
                    );
                    let at = self.at(*origin);
                    let height = px(*line_height);
                    let layout = layout.unwrap_or_default();
                    let align = text_align(layout.align);
                    let align_width = layout.width.map(px);
                    let _ = line.paint_background(at, height, align, align_width, window, cx);
                    let mut report =
                        underline_tag
                            .filter(|_| self.underlines.is_some())
                            .map(|tag| LineUnderlines {
                                tag,
                                segments: Vec::new(),
                            });
                    let painted = line.paint_with_underline_handler(
                        at,
                        height,
                        align,
                        align_width,
                        window,
                        cx,
                        |range, start, width, native, window| {
                            let range = [range.start, range.end];
                            if let Some(report) = &mut report {
                                let local = start - self.canvas_origin;
                                let gpui::Rgba { r, g, b, a } =
                                    native.color.unwrap_or_default().into();
                                report.segments.push(UnderlineGeometry {
                                    range,
                                    origin: [f32::from(local.x), f32::from(local.y)],
                                    width: f32::from(width),
                                    thickness: f32::from(native.thickness),
                                    color: [r, g, b, a],
                                    wavy: native.wavy,
                                });
                            }
                            let rule = underlines.iter().find(|rule| rule.range == range);
                            if rule.is_some_and(|rule| rule.skip) {
                                return;
                            }
                            let mut style = rule
                                .and_then(|rule| rule.style.as_ref())
                                .map_or(*native, |style| self.underline(style));
                            style.color = style.color.or(native.color);
                            let exclusions =
                                rule.map_or(&[][..], |rule| rule.exclusions.as_slice());
                            Self::paint_underline(start, width, &style, exclusions, window);
                        },
                    );
                    if painted.is_ok()
                        && let Some(report) = report
                    {
                        self.underlines
                            .as_mut()
                            .expect("collecting underlines")
                            .push(report);
                    }
                }
                Draw::Underline {
                    origin,
                    width,
                    style,
                    exclusions,
                } => {
                    Self::paint_underline(
                        self.at(*origin),
                        px(*width),
                        &self.underline(style),
                        exclusions,
                        window,
                    );
                }
                Draw::Glyphs {
                    baseline,
                    text,
                    font: family,
                    font_options,
                    size,
                    color,
                } => {
                    let size = px(*size);
                    let ink = self.ink(*color);
                    let runs = [TextRun {
                        len: text.len(),
                        font: native_font(family.as_deref(), font_options.as_ref(), style(cx).font),
                        color: ink,
                        ..TextRun::default()
                    }];
                    let line =
                        window
                            .text_system()
                            .shape_line(text.clone().into(), size, &runs, None);
                    for run in &line.runs {
                        for glyph in &run.glyphs {
                            let origin = self.at(*baseline) + glyph.position;
                            if glyph.is_emoji {
                                let _ = window.paint_emoji(origin, run.font_id, glyph.id, size);
                            } else {
                                let _ =
                                    window.paint_glyph(origin, run.font_id, glyph.id, size, ink);
                            }
                        }
                    }
                }
                Draw::Strike {
                    origin,
                    width,
                    style,
                } => window.paint_strikethrough(self.at(*origin), px(*width), &self.strike(style)),
                Draw::Svg {
                    bounds,
                    asset,
                    color,
                    scale,
                    rotation,
                    offset,
                    transform,
                } => {
                    let Some(Asset::Svg { id, bytes }) = self.resources.assets.get(asset) else {
                        continue;
                    };
                    let bounds = self.rect(*bounds);
                    let scale_factor = window.scale_factor();
                    // GPUI's SVG element transforms around its pixel-snapped layout bounds.
                    let bounds = Bounds::from_corners(
                        window.pixel_snap_point(bounds.origin),
                        window.pixel_snap_point(bounds.bottom_right()),
                    );
                    let center = bounds.center().scale(scale_factor);
                    let relative = center.map(|ScaledPixels(value)| ScaledPixels(-value));
                    let transform = if let Some([a, b, c, d, x, y]) = transform {
                        TransformationMatrix::unit()
                            .translate(center)
                            .compose(TransformationMatrix {
                                rotation_scale: [[*a, *c], [*b, *d]],
                                translation: [x * scale_factor, y * scale_factor],
                            })
                            .translate(relative)
                    } else {
                        let offset =
                            p(offset.unwrap_or([0., 0.])).map(|value| value.scale(scale_factor));
                        let [x, y] = scale.unwrap_or([1., 1.]);
                        TransformationMatrix::unit()
                            .translate(center + offset)
                            .rotate(radians(rotation.unwrap_or(0.)))
                            .scale(size(x, y))
                            .translate(relative)
                    };
                    let painted = window.paint_svg(
                        bounds,
                        id.clone(),
                        Some(bytes),
                        transform,
                        self.ink(*color),
                        cx,
                    );
                    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
                    if painted.is_ok() {
                        crate::ui::native_atlas::mark_svg(window, cx, bounds, id);
                    }
                    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
                    let _ = painted;
                }
                Draw::Image {
                    bounds,
                    image_bounds,
                    asset,
                    frame,
                    corners: radii,
                    grayscale,
                } => {
                    let Some(Asset::Image(image)) = self.resources.assets.get(asset) else {
                        continue;
                    };
                    let _ = window.paint_image(
                        self.rect(*bounds),
                        self.rect(image_bounds.unwrap_or(*bounds)),
                        corners(*radii),
                        Arc::clone(image),
                        *frame,
                        *grayscale,
                    );
                }
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                Draw::AppleSurface { bounds, asset, .. } => {
                    if let Some(Asset::AppleSurface(surface)) = self.resources.assets.get(asset) {
                        window.paint_surface(self.rect(*bounds), surface.retain_for_paint());
                    }
                }
                #[cfg(not(any(target_os = "macos", target_os = "ios")))]
                Draw::AppleSurface { .. } => {}
                Draw::Group {
                    clip,
                    offset,
                    absolute_offset,
                    layer,
                    opacity,
                    draw,
                } => {
                    let origin = self.origin;
                    self.origin = match absolute_offset {
                        Some(offset) => self.canvas_origin + p(*offset),
                        None => origin + p(offset.unwrap_or([0., 0.])),
                    };
                    let mask = clip.map(|rect| ContentMask {
                        bounds: self.rect(rect),
                    });
                    let layer = layer.map(|rect| self.rect(rect));
                    window.with_content_mask(mask, |window| {
                        let mut paint = |window: &mut Window, cx: &mut App| {
                            if let Some(layer) = layer {
                                window.paint_layer(layer, |window| self.paint(draw, window, cx));
                            } else {
                                self.paint(draw, window, cx);
                            }
                        };
                        if let Some(opacity) = opacity {
                            // Native element opacity applies to images, color glyphs, and paint colors.
                            let mut group = Interactivity::default();
                            group.base_style.opacity = Some(*opacity);
                            group.paint(
                                None,
                                None,
                                window.content_mask().bounds,
                                None,
                                window,
                                cx,
                                |_, window, cx| paint(window, cx),
                            );
                        } else {
                            paint(window, cx);
                        }
                    });
                    self.origin = origin;
                }
            }
        }
    }
}

fn text_align(alignment: TextAlignment) -> TextAlign {
    match alignment {
        TextAlignment::Left => TextAlign::Left,
        TextAlignment::Center => TextAlign::Center,
        TextAlignment::Right => TextAlign::Right,
    }
}

fn p([x, y]: [f32; 2]) -> Point<Pixels> {
    point(px(x), px(y))
}

fn corners([top_left, top_right, bottom_right, bottom_left]: [f32; 4]) -> Corners<Pixels> {
    Corners {
        top_left: px(top_left),
        top_right: px(top_right),
        bottom_right: px(bottom_right),
        bottom_left: px(bottom_left),
    }
}
