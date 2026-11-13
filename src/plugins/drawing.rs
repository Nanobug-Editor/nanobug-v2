//! Prepare the native Canvas geometry and package assets on bounded background threads.

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;
mod font;

use super::package::{InstallStore, Release, hash_bytes};
use anyhow::{Context, Result, ensure};
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleSurface;
use font::FontLine;
use gpui::{
    FillOptions, Path, PathBuilder, Pixels, RenderImage, SharedString, StrokeOptions, Transform,
    point, px,
};
use image::{AnimationDecoder, Frame, ImageDecoder, ImageFormat, ImageReader, Limits};
use nanobug_plugin_sdk::canvas::{
    Canvas, Draw, FillRule, LineCap, LineJoin, MeasureText, PackageFont, PathStyle, Point, Segment,
};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    io::{BufReader, Cursor},
    sync::Arc,
};

const IMAGE_MEMORY: usize = 24 * 1024 * 1024;
const FONT_MEMORY: u64 = 24 * 1024 * 1024;
const MAX_VERTICES: usize = 48_000;

/// A complete scene with its resolved assets and pre-tessellated paths in command order.
#[derive(Debug)]
pub struct PreparedCanvas {
    pub paths: Vec<Path<Pixels>>,
    pub assets: BTreeMap<String, Asset>,
    pub resource_bytes: usize,
    /// Whether a tagged native line will populate the next-frame reply.
    pub reports_underlines: bool,
}

/// A resource owned by this verified package and retained only with live scenes.
#[derive(Debug)]
pub enum Asset {
    Svg {
        id: SharedString,
        bytes: Vec<u8>,
    },
    Image(Arc<RenderImage>),
    Font(Vec<u8>),
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    AppleSurface(AppleSurface),
}

/// Check every resource and native path before making a scene visible.
pub(super) fn prepare(
    store: &InstallStore,
    release: &Release,
    canvas: &Canvas,
) -> Result<PreparedCanvas> {
    ensure!(canvas.validate(), "Invalid or excessive drawing commands");
    let mut result = PreparedCanvas {
        paths: Vec::new(),
        assets: BTreeMap::new(),
        resource_bytes: 0,
        reports_underlines: canvas.reports_underlines(),
    };
    let mut image_bytes = 0;
    let mut font_bytes = 0;
    let mut apple_sizes = BTreeMap::new();
    collect_apple_surfaces(&canvas.draw, &mut apple_sizes)?;
    for name in canvas.resources() {
        let asset = if let Some(&dimensions) = apple_sizes.get(name) {
            load_apple_surface(store, release, name, dimensions, &mut image_bytes)?
        } else if PackageFont::is_font_asset(name) {
            let bytes = store.verified_asset_bytes(release, name, FONT_MEMORY - font_bytes)?;
            font_bytes += bytes.len() as u64;
            Asset::Font(bytes)
        } else if name.ends_with(".svg") {
            let bytes = store.verified_asset_bytes(release, name, 256 * 1024)?;
            let text = std::str::from_utf8(&bytes)?;
            let xml = roxmltree::Document::parse(text).context("Invalid SVG")?;
            ensure!(
                xml.root_element().tag_name().name() == "svg",
                "Expected an SVG document"
            );
            ensure!(xml.descendants().count() <= 4096, "SVG has too many nodes");
            ensure!(
                xml.descendants()
                    .filter(|node| node.is_element())
                    .all(|node| {
                        node.attributes().all(|attribute| {
                            attribute.name() != "href" || attribute.value().starts_with('#')
                        })
                    }),
                "Plugin SVG references must stay in the document"
            );
            Asset::Svg {
                id: format!("nanobug-plugin-svg:{}", hash_bytes(&bytes)).into(),
                bytes,
            }
        } else {
            let bytes = store.verified_asset_bytes(release, name, 4 * 1024 * 1024)?;
            Asset::Image(Arc::new(decode_image(bytes, &mut image_bytes)?))
        };
        result.assets.insert(name.to_owned(), asset);
    }
    let mut vertices = 0;
    prepare_draw(&canvas.draw, &mut result, &mut vertices)?;
    // Fonts have been reduced to outlines. No plugin font remains registered or cached globally.
    result
        .assets
        .retain(|_, asset| !matches!(asset, Asset::Font(_)));
    result.resource_bytes =
        image_bytes + vertices * std::mem::size_of::<gpui::PathVertex<Pixels>>();
    result.resource_bytes += result
        .assets
        .values()
        .filter_map(|asset| match asset {
            Asset::Svg { bytes, .. } => Some(bytes.len()),
            _ => None,
        })
        .sum::<usize>();
    Ok(result)
}

/// Check repeated references to a planar asset before creating the native surface.
fn collect_apple_surfaces<'a>(
    draw: &'a [Draw],
    sizes: &mut BTreeMap<&'a str, [u16; 2]>,
) -> Result<()> {
    for command in draw {
        match command {
            Draw::AppleSurface {
                asset, dimensions, ..
            } => {
                ensure!(
                    cfg!(any(target_os = "macos", target_os = "ios")),
                    "Apple Canvas surfaces are unavailable on this platform"
                );
                if let Some(previous) = sizes.insert(asset, *dimensions) {
                    ensure!(
                        previous == *dimensions,
                        "Conflicting dimensions for an NV12 asset"
                    );
                }
            }
            Draw::Group { draw, .. } => collect_apple_surfaces(draw, sizes)?,
            _ => {}
        }
    }
    Ok(())
}

/// Allocate a native surface from exact package data and charge its padded planes to the image budget.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn load_apple_surface(
    store: &InstallStore,
    release: &Release,
    asset: &str,
    [width, height]: [u16; 2],
    allocated: &mut usize,
) -> Result<Asset> {
    let packed_size = u64::from(width) * u64::from(height) * 3 / 2;
    let bytes = store.verified_asset_bytes(release, asset, packed_size)?;
    let surface = AppleSurface::from_nv12([width, height], &bytes)?;
    *allocated += surface.bytes;
    ensure!(
        *allocated <= IMAGE_MEMORY,
        "Plugin scene exceeds the decoded image budget"
    );
    Ok(Asset::AppleSurface(surface))
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn load_apple_surface(
    _store: &InstallStore,
    _release: &Release,
    _asset: &str,
    _dimensions: [u16; 2],
    _allocated: &mut usize,
) -> Result<Asset> {
    anyhow::bail!("Apple Canvas surfaces are unavailable on this platform")
}

/// Shape the package-font lines off the UI thread. Native-font lines keep their positions in the batch.
pub(super) fn prepare_text_measurements(
    store: &InstallStore,
    release: &Release,
    lines: &[MeasureText],
) -> Result<Vec<Option<serde_json::Value>>> {
    let mut assets = BTreeMap::new();
    let mut remaining = FONT_MEMORY;
    lines
        .iter()
        .map(|line| {
            let Some(font) = &line.package_font else {
                return Ok(None);
            };
            let bytes = match assets.entry(font.asset.as_str()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let bytes = store.verified_asset_bytes(release, &font.asset, remaining)?;
                    remaining -= bytes.len() as u64;
                    entry.insert(bytes)
                }
            };
            let layout = FontLine::shape(bytes, font.face, &line.text, line.size)?;
            Ok(Some(layout.measure(&line.text)))
        })
        .collect()
}

fn prepare_draw(draw: &[Draw], scene: &mut PreparedCanvas, vertices: &mut usize) -> Result<()> {
    for command in draw {
        match command {
            Draw::Path {
                segments,
                stroke,
                style,
                dash,
                transform,
                ..
            } => {
                let path = std::panic::catch_unwind(|| {
                    build_path(segments, *stroke, *style, dash, *transform)
                })
                .map_err(|_| anyhow::anyhow!("Invalid plugin path geometry"))??;
                push_path(scene, vertices, path)?;
            }
            Draw::PackageText {
                baseline,
                text,
                font,
                size,
                ..
            } => {
                let Some(Asset::Font(bytes)) = scene.assets.get(&font.asset) else {
                    anyhow::bail!("Package text requires a font asset");
                };
                let path = std::panic::catch_unwind(|| {
                    FontLine::shape(bytes, font.face, text, *size)?.outline(*baseline)
                })
                .map_err(|_| anyhow::anyhow!("Invalid plugin font outline"))??;
                push_path(scene, vertices, path)?;
            }
            Draw::Svg { asset, bounds, .. } => {
                ensure!(
                    bounds[2] <= 1024. && bounds[3] <= 1024.,
                    "SVG draw size exceeds the plugin image budget"
                );
                ensure!(
                    matches!(scene.assets.get(asset), Some(Asset::Svg { .. })),
                    "SVG command requires an SVG asset"
                );
            }
            Draw::Image { asset, frame, .. } => {
                ensure!(
                    matches!(scene.assets.get(asset), Some(Asset::Image(image)) if *frame < image.frame_count()),
                    "Plugin image frame does not exist"
                );
            }
            Draw::Group { draw, .. } => prepare_draw(draw, scene, vertices)?,
            _ => {}
        }
    }
    Ok(())
}

/// Retain one tessellated path in paint order after checking the whole scene's vertex budget.
fn push_path(scene: &mut PreparedCanvas, vertices: &mut usize, path: Path<Pixels>) -> Result<()> {
    *vertices += path.vertices.len();
    ensure!(
        *vertices <= MAX_VERTICES,
        "Drawing exceeds the path vertex budget"
    );
    scene.paths.push(path);
    Ok(())
}

fn build_path(
    segments: &[Segment],
    stroke: Option<f32>,
    style: Option<PathStyle>,
    dash: &[f32],
    transform: Option<[f32; 6]>,
) -> Result<Path<Pixels>> {
    let mut path = stroke.map_or_else(PathBuilder::fill, |width| PathBuilder::stroke(px(width)));
    if let Some(style) = style {
        path = path.with_style(native_path_style(style));
    }
    if !dash.is_empty() {
        ensure!(
            segments.len() <= 128 && matches!(path.style, gpui::PathStyle::Stroke(_)),
            "Dashed paths require a bounded stroked path"
        );
        path = path.dash_array(&dash.iter().copied().map(px).collect::<Vec<_>>());
    }
    if let Some([m11, m12, m21, m22, m31, m32]) = transform {
        ensure!(
            [m11, m12, m21, m22].iter().all(|value| value.abs() <= 32.),
            "Path transformation is too large"
        );
        path.transform(Transform::new(m11, m12, m21, m22, m31, m32));
    }
    for segment in segments {
        match segment {
            Segment::Move { to } => path.move_to(p(*to)),
            Segment::Line { to } => path.line_to(p(*to)),
            Segment::Quadratic { to, control } => path.curve_to(p(*to), p(*control)),
            Segment::Cubic { to, first, second } => {
                path.cubic_bezier_to(p(*to), p(*first), p(*second))
            }
            Segment::Arc {
                to,
                radii,
                rotation,
                large,
                sweep,
            } => path.arc_to(p(*radii), px(*rotation), *large, *sweep, p(*to)),
            Segment::Polygon { points, closed } => {
                path.add_polygon(&points.iter().copied().map(p).collect::<Vec<_>>(), *closed)
            }
            Segment::Close => path.close(),
        }
    }
    path.build()
}

/// Translate SDK styling to the options used by the pinned GPUI tessellator.
fn native_path_style(style: PathStyle) -> gpui::PathStyle {
    match style {
        PathStyle::Fill { rule, tolerance } => gpui::PathStyle::Fill(
            FillOptions::default()
                .with_fill_rule(match rule {
                    FillRule::EvenOdd => gpui::FillRule::EvenOdd,
                    FillRule::NonZero => gpui::FillRule::NonZero,
                })
                .with_tolerance(tolerance.unwrap_or(FillOptions::DEFAULT_TOLERANCE)),
        ),
        PathStyle::Stroke {
            width,
            start_cap,
            end_cap,
            join,
            miter_limit,
            tolerance,
        } => {
            let cap = |cap| match cap {
                LineCap::Butt => lyon_path::LineCap::Butt,
                LineCap::Square => lyon_path::LineCap::Square,
                LineCap::Round => lyon_path::LineCap::Round,
            };
            gpui::PathStyle::Stroke(
                StrokeOptions::default()
                    .with_line_width(width)
                    .with_start_cap(cap(start_cap))
                    .with_end_cap(cap(end_cap))
                    .with_line_join(match join {
                        LineJoin::Miter => lyon_path::LineJoin::Miter,
                        LineJoin::MiterClip => lyon_path::LineJoin::MiterClip,
                        LineJoin::Round => lyon_path::LineJoin::Round,
                        LineJoin::Bevel => lyon_path::LineJoin::Bevel,
                    })
                    .with_miter_limit(miter_limit.unwrap_or(StrokeOptions::DEFAULT_MITER_LIMIT))
                    .with_tolerance(tolerance.unwrap_or(StrokeOptions::DEFAULT_TOLERANCE)),
            )
        }
    }
}

fn p([x, y]: Point) -> gpui::Point<Pixels> {
    point(px(x), px(y))
}

fn decode_image(bytes: Vec<u8>, allocated: &mut usize) -> Result<RenderImage> {
    let format = image::guess_format(&bytes)?;
    ensure!(
        matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
        ),
        "Plugin images use PNG, JPEG, GIF or WebP"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(IMAGE_MEMORY as u64);
    let data = BufReader::new(Cursor::new(bytes));
    let mut frames = smallvec::SmallVec::<[Frame; 1]>::new();
    if format == ImageFormat::Gif {
        let mut decoder = image::codecs::gif::GifDecoder::new(data)?;
        decoder.set_limits(limits)?;
        for frame in decoder.into_frames() {
            ensure!(frames.len() < 48, "Plugin image has too many frames");
            frames.push(convert_frame(frame?, allocated)?);
        }
    } else {
        let mut reader = ImageReader::with_format(data, format);
        reader.limits(limits);
        frames.push(convert_frame(
            Frame::new(reader.decode()?.into_rgba8()),
            allocated,
        )?);
    }
    ensure!(!frames.is_empty(), "Plugin image has no frames");
    Ok(RenderImage::new(frames))
}

fn convert_frame(frame: Frame, allocated: &mut usize) -> Result<Frame> {
    let delay = frame.delay();
    let mut buffer = frame.into_buffer();
    *allocated += buffer.as_raw().len();
    ensure!(
        *allocated <= IMAGE_MEMORY,
        "Plugin scene exceeds the decoded image budget"
    );
    for pixel in buffer.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Frame::from_parts(buffer, 0, 0, delay))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_path_matches_native_caps_joins_and_fill_rule() -> Result<()> {
        let scene: Canvas = serde_json::from_str(
            r#"{"draw":[
                {"op":"path","segments":[{"segment":"move","to":[10,10]},
                    {"segment":"line","to":[30,10]},{"segment":"line","to":[30,35]}],
                 "fill":{"fill":"solid","color":4294967295},
                 "style":{"kind":"stroke","width":6,"start_cap":"square",
                     "end_cap":"round","join":"bevel","miter_limit":3,"tolerance":0.2}},
                {"op":"path","segments":[
                     {"segment":"polygon","points":[[0,0],[40,0],[40,40],[0,40]],"closed":true},
                     {"segment":"polygon","points":[[10,10],[30,10],[30,30],[10,30]],"closed":true}],
                 "fill":{"fill":"solid","color":4294967295},
                 "style":{"kind":"fill","rule":"non_zero","tolerance":0.2}}
            ]}"#,
        )?;
        assert!(scene.validate());
        let mut actual = PreparedCanvas {
            paths: Vec::new(),
            assets: BTreeMap::new(),
            resource_bytes: 0,
            reports_underlines: false,
        };
        prepare_draw(&scene.draw, &mut actual, &mut 0)?;

        let mut stroke = PathBuilder::fill().with_style(gpui::PathStyle::Stroke(
            gpui::StrokeOptions::default()
                .with_line_width(6.)
                .with_start_cap(lyon_path::LineCap::Square)
                .with_end_cap(lyon_path::LineCap::Round)
                .with_line_join(lyon_path::LineJoin::Bevel)
                .with_miter_limit(3.)
                .with_tolerance(0.2),
        ));
        stroke.move_to(p([10., 10.]));
        stroke.line_to(p([30., 10.]));
        stroke.line_to(p([30., 35.]));
        let mut fill = PathBuilder::fill().with_style(gpui::PathStyle::Fill(
            gpui::FillOptions::non_zero().with_tolerance(0.2),
        ));
        fill.add_polygon(&[[0., 0.], [40., 0.], [40., 40.], [0., 40.]].map(p), true);
        fill.add_polygon(
            &[[10., 10.], [30., 10.], [30., 30.], [10., 30.]].map(p),
            true,
        );
        for (actual, expected) in actual.paths.iter().zip([stroke.build()?, fill.build()?]) {
            assert_eq!(actual.bounds, expected.bounds);
            assert!(
                actual
                    .vertices
                    .iter()
                    .map(|vertex| vertex.xy_position)
                    .eq(expected.vertices.iter().map(|vertex| vertex.xy_position))
            );
        }
        assert_eq!(actual.paths.len(), 2);
        Ok(())
    }
}
