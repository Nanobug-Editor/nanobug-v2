//! Shared SVG descriptors and native raster caching, independent of concrete grid windows.

use gpui::{
    App, Bounds, Corners, DevicePixels, Global, ObjectFit, Pixels, RenderImage, SvgRenderer,
    SvgSize, Window, point, px, size,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

/// Immutable SVG source and its intrinsic aspect ratio, shared by frame and backdrop painters.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Icon {
    /// Embedded source bytes; their stable address identifies the asset without hashing its contents.
    source: &'static [u8],
    /// Intrinsic width divided by height.
    aspect: f32,
}

impl PartialEq for Icon {
    /// Compare source identity and aspect with `other` without scanning SVG contents.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.source, other.source) && self.aspect.to_bits() == other.aspect.to_bits()
    }
}
impl Eq for Icon {}
impl Hash for Icon {
    /// Feed the same source identity and aspect used by equality into the supplied `state`.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.as_ptr().hash(state);
        self.source.len().hash(state);
        self.aspect.to_bits().hash(state);
    }
}

impl Icon {
    /// Describe embedded SVG `source` with the original `aspect` ratio; malformed data fails at rasterization.
    pub(crate) const fn new(source: &'static [u8], aspect: f32) -> Self {
        Self { source, aspect }
    }

    /// Rasterize with `svg`, fitting the original colors within `edge` device pixels.
    pub(crate) fn rasterize(self, svg: &SvgRenderer, edge: i32) -> Option<Arc<RenderImage>> {
        let parsed = svg.parse_svg(self.source).ok()?;
        let width = (edge as f32 * self.aspect.min(1.)).round().max(1.) as i32;
        let height = (edge as f32 / self.aspect.max(1.)).round().max(1.) as i32;
        svg.render_parsed(
            &parsed,
            SvgSize::ExactSize(size(DevicePixels(width), DevicePixels(height))),
        )
        .ok()
    }
}

/// Visible height of a keyboard legend made of letters or digits, in logical pixels.
const TEXT_KEY_HEIGHT: f32 = 9.;
/// Visible height of a keyboard pictogram, in logical pixels.
const PICTOGRAM_KEY_HEIGHT: f32 = 12.;

/// SVG geometry for an inline keyboard hint aligned by its visible path.
#[derive(Clone, Copy)]
pub(crate) struct KeyIcon {
    /// The original SVG and its full canvas aspect ratio.
    pub(crate) image: Icon,
    /// Width of the scaled path in logical pixels, excluding transparent margins.
    pub(crate) visible_width: f32,
    /// Scaled canvas x, y, width and height relative to the path's left edge and vertical center.
    canvas: [f32; 4],
}

impl KeyIcon {
    /// Lettered Escape legend shared by the leader and language footers.
    pub(crate) const ESC: Self = Self::new(
        include_bytes!("../../assets/icons/leader-close.svg"),
        [24., 24.],
        [1., 7., 22., 10.],
        TEXT_KEY_HEIGHT,
    );
    /// Backspace pictogram for returning to the parent leader group.
    pub(crate) const BACKSPACE: Self = Self::new(
        include_bytes!("../../assets/icons/leader-back.svg"),
        [16., 16.],
        [1., 3., 13., 10.],
        PICTOGRAM_KEY_HEIGHT,
    );
    /// Return arrow from the WhichKey glyph used in the language footer.
    pub(crate) const ENTER: Self = Self::new(
        include_bytes!("../../assets/icons/key-enter.svg"),
        [24., 24.],
        [1.5, 5.366, 21., 13.268],
        PICTOGRAM_KEY_HEIGHT,
    );

    /// Scale `source` to a visible `height` using its `canvas` size and canvas-relative `ink` bounds.
    const fn new(source: &'static [u8], canvas: [f32; 2], ink: [f32; 4], height: f32) -> Self {
        let [width, view_height] = canvas;
        let [left, top, ink_width, ink_height] = ink;
        let scale = height / ink_height;
        Self {
            image: Icon::new(source, width / view_height),
            visible_width: ink_width * scale,
            canvas: [
                -left * scale,
                -top * scale - height / 2.,
                width * scale,
                view_height * scale,
            ],
        }
    }

    /// Whole grid columns of `cell_width` needed for the visible path and trailing `gap`.
    pub(crate) fn columns(self, cell_width: f32, gap: f32) -> usize {
        ((self.visible_width + gap) / cell_width).ceil() as usize
    }

    /// SVG canvas bounds with its visible left edge shifted by `offset` from the row anchor.
    pub(crate) fn canvas(self, offset: f32) -> Bounds<Pixels> {
        let [x, y, w, h] = self.canvas;
        Bounds::new(point(px(x + offset), px(y)), size(px(w), px(h)))
    }
}

/// Retains color sprites across frames, shared by the workspace panels.
#[derive(Default)]
pub(crate) struct IconCache(RefCell<IconRasters>);

impl Global for IconCache {}

/// One window's icon rasters at its current display scale.
#[derive(Default)]
struct IconRasters {
    /// Scale at which the cached sprites were rasterized.
    scale: f32,
    /// Successful or failed rasters keyed by icon and longest edge in device pixels.
    images: HashMap<(Icon, i32), Option<Arc<RenderImage>>>,
}

impl IconCache {
    /// Paint `icon` within logical `bounds`, using `window`'s current DPI and `cx`'s SVG renderer.
    pub(crate) fn paint(&self, icon: Icon, bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
        let scale = window.scale_factor();
        let edge = (f32::from(bounds.size.width.max(bounds.size.height)) * scale)
            .round()
            .max(1.) as i32;
        let mut rasters = self.0.borrow_mut();
        if rasters.scale != scale {
            for image in rasters.images.drain().filter_map(|(_, image)| image) {
                let _ = window.drop_image(image);
            }
            rasters.scale = scale;
        }
        let image = rasters
            .images
            .entry((icon, edge))
            .or_insert_with(|| icon.rasterize(&cx.svg_renderer(), edge));
        if let Some(image) = image {
            let image_bounds = ObjectFit::Contain.get_bounds(bounds, image.size(0));
            let _ = window.paint_image(
                bounds,
                image_bounds,
                Corners::default(),
                image.clone(),
                0,
                false,
            );
        }
    }
}
