//! Rasterize the workspace layers once for the shortcut popup's frosted backdrop.
use crate::{ui::icons::Icon, ui::theme::BACKGROUND};
use gpui::{
    Bounds, DevicePixels, ObjectFit, Pixels, PlatformTextSystem, Point, RenderGlyphParams,
    RenderImage, Size, SvgRenderer, point, size,
};
use image::{GrayImage, Pixel, Rgba, RgbaImage, imageops};
use std::{collections::HashMap, sync::Arc};

/// Blur radius in logical pixels.
const BLUR_SIGMA: f32 = 4.5;
/// Keep the cached texture small even on a large or high-DPI window.
const MAX_RASTER_EDGE: f32 = 1280.;
/// Opacity of the translucent wash using the shared window tint.
const WASH_ALPHA: u8 = 54;

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

    /// Stamp a glyph at its baseline `at` in RGB `color`, constrained by the pane's `clip`.
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
    pub(crate) fn finish(mut self, popup: Bounds<Pixels>, radius: f32) -> Arc<RenderImage> {
        for pixel in self.pixels.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            for channel in &mut pixel.0[..3] {
                *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
        let mut blurred = imageops::fast_blur(&self.pixels, BLUR_SIGMA * self.scale);
        let mut wash = BACKGROUND.to_be_bytes();
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

/// Blend a glyph `mask` into `pixels` at logical `at`, in RGB `color`, inside raster `clip`.
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
    let mut rgba = (color << 8).to_be_bytes();
    for y in area.top().0..area.bottom().0 {
        for x in area.left().0..area.right().0 {
            rgba[3] = mask
                .pixels
                .get_pixel((x - origin.x.0) as u32, (y - origin.y.0) as u32)[0];
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
    use gpui::px;

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
        let image = raster.finish(popup, 12.);
        let pixels = image.as_bytes(0).unwrap();
        let alpha = |x: usize, y: usize| pixels[(y * 50 + x) * 4 + 3];
        assert!(alpha(11, 4) > WASH_ALPHA);
        assert_eq!(alpha(25, 25), 0);
        assert!(alpha(14, 14) >= WASH_ALPHA);
    }
}
