//! Rasterize the workspace layers once for a popup's frosted backdrop.
use crate::ui::icons::Icon;
use gpui::{
    Bounds, DevicePixels, ObjectFit, Path, Pixels, PlatformTextSystem, Point, RenderGlyphParams,
    RenderImage, Size, SvgRenderer, point, size,
};
use image::{GrayImage, ImageBuffer, Luma, Pixel, Rgba, RgbaImage, imageops};
use std::{collections::HashMap, sync::Arc};

/// Blur radius in logical pixels.
const BLUR_SIGMA: f32 = 4.5;
/// Keep the cached texture small even on a large or high-DPI window.
const MAX_RASTER_EDGE: f32 = 1280.;
/// Opacity of the translucent wash using the shared window tint.
const WASH_ALPHA: u8 = 54;
/// Subpixel sample positions on both axes for each downsampled path pixel.
const PATH_SAMPLES: [f32; 4] = [0.125, 0.375, 0.625, 0.875];

/// Coverage bitmap and its offset from a text glyph's baseline.
struct Mask {
    /// Offset in raster pixels.
    origin: Point<DevicePixels>,
    /// Per-pixel alpha coverage.
    pixels: GrayImage,
}

/// Composes the existing grid's ink and fills into one transparent image.
pub(crate) struct BackdropRaster<'a> {
    /// Straight-alpha RGBA workspace layer.
    pixels: RgbaImage,
    /// Raster pixels per logical pixel.
    scale: f32,
    /// Native font rasterizer used by the normal editor.
    text: &'a dyn PlatformTextSystem,
    /// Rasterized glyphs reused across panes for this snapshot.
    glyphs: HashMap<RenderGlyphParams, Option<Mask>>,
    /// SVG renderer for the built-in file icons.
    svg: SvgRenderer,
    /// BGRA file icons at the snapshot's scale and shared grid font size.
    icons: HashMap<Icon, Option<Arc<RenderImage>>>,
}

impl<'a> BackdropRaster<'a> {
    /// Allocate a downsampled raster for `viewport` using the editor's `text` and `svg` renderers.
    pub(crate) fn new(
        viewport: Size<Pixels>,
        text: &'a dyn PlatformTextSystem,
        svg: SvgRenderer,
    ) -> Self {
        let width = f32::from(viewport.width);
        let height = f32::from(viewport.height);
        let scale = 0.5_f32.min(MAX_RASTER_EDGE / width.max(height).max(1.));
        Self {
            pixels: RgbaImage::new(
                (width * scale).ceil().max(1.) as u32,
                (height * scale).ceil().max(1.) as u32,
            ),
            scale,
            text,
            glyphs: HashMap::new(),
            svg,
            icons: HashMap::new(),
        }
    }

    /// Convert logical `bounds` to raster pixels clipped to the allocated image.
    fn pixel_clip(&self, bounds: Bounds<Pixels>) -> Bounds<DevicePixels> {
        let from = bounds
            .origin
            .map(|v| DevicePixels((f32::from(v) * self.scale).round() as i32));
        let to = bounds
            .bottom_right()
            .map(|v| DevicePixels((f32::from(v) * self.scale).round() as i32));
        Bounds::from_corners(from, to).intersect(&Bounds::new(
            point(DevicePixels(0), DevicePixels(0)),
            size(
                DevicePixels(self.pixels.width() as i32),
                DevicePixels(self.pixels.height() as i32),
            ),
        ))
    }

    /// Blend a retained `color` fill into the visible part of `bounds`.
    pub(crate) fn fill(&mut self, bounds: Bounds<Pixels>, color: u32) {
        let area = self.pixel_clip(bounds);
        let source = Rgba(color.to_be_bytes());
        for y in area.top().0..area.bottom().0 {
            for x in area.left().0..area.right().0 {
                self.pixels.get_pixel_mut(x as u32, y as u32).blend(&source);
            }
        }
    }

    /// Blend the tessellated `path` into the raster in RGBA `color`, within the panel `clip`.
    pub(crate) fn path(&mut self, path: &Path<Pixels>, clip: Bounds<Pixels>, color: u32) {
        let visible = self.pixel_clip(clip);
        let area = triangle_bounds(&[
            path.bounds
                .origin
                .map(|value| f32::from(value) * self.scale),
            path.bounds
                .bottom_right()
                .map(|value| f32::from(value) * self.scale),
        ])
        .intersect(&visible);
        if area.is_empty() {
            return;
        }
        let mut coverage = ImageBuffer::<Luma<u16>, Vec<u16>>::new(
            area.size.width.0 as u32,
            area.size.height.0 as u32,
        );
        // Combine samples across triangles before blending once so shared edges stay seamless.
        for triangle in path.vertices.as_chunks::<3>().0 {
            let triangle = triangle.each_ref().map(|vertex| {
                vertex
                    .xy_position
                    .map(|value| f32::from(value) * self.scale)
            });
            let [a, b, c] = triangle;
            let winding = edge(a, b, c);
            if winding == 0. {
                continue;
            }
            let winding = winding.signum();
            let bounds = triangle_bounds(&triangle).intersect(&area);
            for y in bounds.top().0..bounds.bottom().0 {
                for x in bounds.left().0..bounds.right().0 {
                    let pixel = coverage
                        .get_pixel_mut((x - area.left().0) as u32, (y - area.top().0) as u32);
                    pixel[0] |= triangle_samples(triangle, winding, x, y);
                }
            }
        }
        let mut source = color.to_be_bytes();
        let opacity = u32::from(source[3]);
        let samples = PATH_SAMPLES.len().pow(2) as u32;
        for (x, y, pixel) in coverage.enumerate_pixels() {
            if pixel[0] != 0 {
                source[3] = ((opacity * pixel[0].count_ones() + samples / 2) / samples) as u8;
                self.pixels
                    .get_pixel_mut(x + area.left().0 as u32, y + area.top().0 as u32)
                    .blend(&Rgba(source));
            }
        }
    }

    /// Blend a triangular badge tip of RGBA `color` in `bounds`, pointing left when `points_left` is set.
    /// Only samples inside `clip` are written.
    pub(crate) fn chevron(
        &mut self,
        bounds: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        points_left: bool,
        color: u32,
    ) {
        let area = self.pixel_clip(bounds.intersect(&clip));
        let source = Rgba(color.to_be_bytes());
        let base = if points_left {
            bounds.right()
        } else {
            bounds.left()
        };
        let base = f32::from(base) * self.scale;
        let center = f32::from(bounds.center().y) * self.scale;
        let half_height = f32::from(bounds.size.height) * self.scale * 0.5;
        let width = f32::from(bounds.size.width) * self.scale;
        for y in area.top().0..area.bottom().0 {
            let reach = width * (1. - (y as f32 + 0.5 - center).abs() / half_height);
            for x in area.left().0..area.right().0 {
                if (x as f32 + 0.5 - base).abs() <= reach {
                    self.pixels.get_pixel_mut(x as u32, y as u32).blend(&source);
                }
            }
        }
    }

    /// Stamp the glyph in `params` at baseline `at` in RGBA `color`, inside the pane's `clip`.
    pub(crate) fn glyph(
        &mut self,
        at: Point<Pixels>,
        mut params: RenderGlyphParams,
        color: u32,
        clip: Bounds<Pixels>,
    ) {
        params.scale_factor = self.scale;
        let visible = self.pixel_clip(clip);
        let mask = self.glyphs.entry(params.clone()).or_insert_with(|| {
            let bounds = self.text.glyph_raster_bounds(&params).ok()?;
            let (size, bytes) = self.text.rasterize_glyph(&params, bounds).ok()?;
            let alpha = if params.is_emoji {
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect()
            } else {
                bytes
            };
            let pixels = GrayImage::from_raw(size.width.0 as u32, size.height.0 as u32, alpha)?;
            Some(Mask {
                origin: bounds.origin,
                pixels,
            })
        });
        if let Some(mask) = mask {
            stamp(&mut self.pixels, self.scale, at, visible, mask, color);
        }
    }

    /// Blend the original colors of `icon` into its logical `bounds`, clipped to its pane.
    pub(crate) fn icon(&mut self, icon: Icon, bounds: Bounds<Pixels>, clip: Bounds<Pixels>) {
        let visible = self.pixel_clip(clip);
        let edge = (f32::from(bounds.size.width) * self.scale).round().max(1.) as i32;
        let image = self
            .icons
            .entry(icon)
            .or_insert_with(|| icon.rasterize(&self.svg, edge));
        let Some(image) = image else { return };
        let pixels = image
            .as_bytes(0)
            .expect("file icon has one frame")
            .as_chunks::<4>()
            .0;
        let image_size = image.size(0);
        let origin = ObjectFit::Contain
            .get_bounds(bounds, image_size)
            .origin
            .map(|v| DevicePixels((f32::from(v) * self.scale).round() as i32));
        let area = Bounds::new(origin, image_size).intersect(&visible);
        let width = image_size.width.0 as usize;
        for y in area.top().0..area.bottom().0 {
            for x in area.left().0..area.right().0 {
                let offset = (y - origin.y.0) as usize * width + (x - origin.x.0) as usize;
                let [b, g, r, a] = pixels[offset];
                self.pixels
                    .get_pixel_mut(x as u32, y as u32)
                    .blend(&Rgba([r, g, b, a]));
            }
        }
    }

    /// Blur the workspace and return a GPUI image with a clear rounded `popup` of radius `radius`.
    pub(crate) fn finish(
        mut self,
        popup: Bounds<Pixels>,
        radius: f32,
        background: u32,
    ) -> Arc<RenderImage> {
        for pixel in self.pixels.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        let mut blurred = imageops::fast_blur(&self.pixels, BLUR_SIGMA * self.scale);
        let mut wash = background.to_be_bytes();
        wash[3] = WASH_ALPHA;
        for (x, y, pixel) in blurred.enumerate_pixels_mut() {
            // Premultiplied color is zero at transparent pixels.
            let alpha = u16::from(pixel[3]).max(1);
            for channel in &mut pixel.0[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
            pixel.blend(&Rgba(wash));
            let at = point((x as f32 + 0.5) / self.scale, (y as f32 + 0.5) / self.scale);
            let outside = rounded_exterior(at, popup, radius, self.scale);
            pixel[3] = (f32::from(pixel[3]) * outside).round() as u8;
            pixel.0.swap(0, 2); // GPUI's color sprite atlas takes BGRA.
        }
        Arc::new(RenderImage::new(vec![image::Frame::new(blurred)]))
    }
}

/// Round the bounds of nonempty `points` in raster space outward so thin strokes retain coverage.
fn triangle_bounds(points: &[Point<f32>]) -> Bounds<DevicePixels> {
    let mut top_left = points[0];
    let mut bottom_right = points[0];
    for p in &points[1..] {
        top_left = point(top_left.x.min(p.x), top_left.y.min(p.y));
        bottom_right = point(bottom_right.x.max(p.x), bottom_right.y.max(p.y));
    }
    Bounds::from_corners(
        top_left.map(|value| DevicePixels(value.floor() as i32)),
        bottom_right.map(|value| DevicePixels(value.ceil() as i32)),
    )
}

/// Return the sample mask in raster pixel `(x, y)` covered by `triangle` with the given `winding`.
fn triangle_samples([a, b, c]: [Point<f32>; 3], winding: f32, x: i32, y: i32) -> u16 {
    let mut samples = 0;
    for (row, dy) in PATH_SAMPLES.into_iter().enumerate() {
        for (col, dx) in PATH_SAMPLES.into_iter().enumerate() {
            let at = point(x as f32 + dx, y as f32 + dy);
            if [edge(a, b, at), edge(b, c, at), edge(c, a, at)]
                .into_iter()
                .all(|distance| winding * distance >= 0.)
            {
                samples |= 1 << (row * PATH_SAMPLES.len() + col);
            }
        }
    }
    samples
}

/// Return the signed area of the triangle whose directed side is `a` to `b` and third point is `at`.
fn edge(a: Point<f32>, b: Point<f32>, at: Point<f32>) -> f32 {
    (b.x - a.x) * (at.y - a.y) - (b.y - a.y) * (at.x - a.x)
}

/// Blend a glyph `mask` at logical `at` in RGBA `color`, using `scale` to map into `pixels` and `clip`.
fn stamp(
    pixels: &mut RgbaImage,
    scale: f32,
    at: Point<Pixels>,
    clip: Bounds<DevicePixels>,
    mask: &Mask,
    color: u32,
) {
    let origin = at.map(|v| DevicePixels((f32::from(v) * scale).round() as i32)) + mask.origin;
    let area = Bounds::new(
        origin,
        size(
            DevicePixels(mask.pixels.width() as i32),
            DevicePixels(mask.pixels.height() as i32),
        ),
    )
    .intersect(&clip);
    let mut rgba = color.to_be_bytes();
    let opacity = u16::from(rgba[3]);
    for y in area.top().0..area.bottom().0 {
        for x in area.left().0..area.right().0 {
            let coverage = mask
                .pixels
                .get_pixel((x - origin.x.0) as u32, (y - origin.y.0) as u32)[0];
            rgba[3] = ((u16::from(coverage) * opacity + 127) / 255) as u8;
            if rgba[3] > 0 {
                pixels.get_pixel_mut(x as u32, y as u32).blend(&Rgba(rgba));
            }
        }
    }
}

/// Coverage outside the rounded `popup` at logical point `at`, smoothed over one raster pixel.
fn rounded_exterior(at: Point<f32>, popup: Bounds<Pixels>, radius: f32, scale: f32) -> f32 {
    let center = popup.center();
    let x = (at.x - f32::from(center.x)).abs() - f32::from(popup.size.width) * 0.5 + radius;
    let y = (at.y - f32::from(center.y)).abs() - f32::from(popup.size.height) * 0.5 + radius;
    let distance = x.max(0.).hypot(y.max(0.)) + x.max(y).min(0.) - radius;
    (distance * scale + 0.5).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{PathBuilder, px};

    /// Downsample tessellated outlines without seams or blending beyond the panel clip.
    #[test]
    fn backdrop_path_keeps_thin_strokes_and_respects_clip() {
        let text = gpui::NoopTextSystem::new();
        let mut raster = BackdropRaster::new(
            size(px(30.), px(30.)),
            &text,
            SvgRenderer::new(Arc::new(())),
        );
        let mut builder = PathBuilder::stroke(px(2.));
        builder.add_polygon(&[point(px(4.), px(8.)), point(px(20.), px(8.))], false);
        let path = builder.build().unwrap();
        let clip = Bounds::new(point(px(8.), px(0.)), size(px(8.), px(30.)));
        raster.path(&path, clip, 0xffffffc0);

        for y in [3, 4] {
            assert_eq!(raster.pixels.get_pixel(3, y)[3], 0);
            for x in 4..8 {
                assert_eq!(raster.pixels.get_pixel(x, y)[3], 96);
            }
            assert_eq!(raster.pixels.get_pixel(8, y)[3], 0);
        }
        assert_eq!(raster.pixels.get_pixel(5, 2)[3], 0);
    }

    /// The mask spreads ink, leaves the popup clear and covers the space outside its round corners.
    #[test]
    fn blurred_backdrop_keeps_the_rounded_popup_clear() {
        let text = gpui::NoopTextSystem::new();
        let mut raster = BackdropRaster::new(
            size(px(100.), px(100.)),
            &text,
            SvgRenderer::new(Arc::new(())),
        );
        raster.fill(
            Bounds::new(point(px(19.), px(8.)), size(px(2.), px(4.))),
            0xffffffff,
        );
        let popup = Bounds::new(point(px(28.), px(28.)), size(px(44.), px(44.)));
        let image = raster.finish(popup, 12., crate::ui::theme::Style::default().background());
        let pixels = image.as_bytes(0).unwrap();
        let alpha = |x: usize, y: usize| pixels[(y * 50 + x) * 4 + 3];
        assert!(alpha(11, 4) > WASH_ALPHA);
        assert_eq!(alpha(25, 25), 0);
        assert!(alpha(14, 14) >= WASH_ALPHA);
    }
}
