//! Compare an installed WASM scene with direct GPUI rendering on each desktop backend.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "linux")]
mod x11;

use super::element_with_underlines;
use anyhow::{Context as _, Result, ensure};
use gpui::{
    App, AppContext, BorderStyle, Bounds, BoxShadow, ColorSpace, ContentMask, Context, Corners,
    Edges, Image, ImageFormat, Interactivity, IntoElement, ParentElement, PathBuilder, PathStyle,
    Pixels, Point, Render, RenderImage, StrikethroughStyle, StrokeOptions, Styled, SvgRenderer,
    TextAlign, TextRun, Transform, Transformation, TransformationMatrix, UnderlineStyle, Window,
    WindowBounds, WindowOptions, canvas, checkerboard, div, fill, font, linear_color_stop,
    linear_gradient, outline, pattern_slash, point, px, quad, radians, rgba, size, svg,
};
use image::{Delay, DynamicImage, Frame, Rgba, RgbaImage, codecs::gif::GifEncoder};
use nanobug::plugins::{
    package::UI_CONTRACT,
    session::{Action, Options, Project, Session, Surface, View},
};
use nanobug_plugin_sdk::canvas::{LineUnderlines, UnderlineGeometry};
use std::{
    cell::RefCell,
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    io::{Cursor, Write},
    path::Path,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use zip::{ZipWriter, write::SimpleFileOptions};

const SCENE: &str = include_str!("combined_scene.json");
const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 30">
  <path d="M2 3 H23 V10 H36 V16 H22 V27 H15 V19 H2 Z"/>
  <circle cx="29" cy="23" r="3"/>
</svg>"#;
const BACKGROUND: u32 = 0x192334ff;
const WIDTH: f32 = 300.;
const HEIGHT: f32 = if cfg!(target_os = "macos") {
    330.
} else {
    260.
};
#[cfg(windows)]
const FONTS: (&str, &str) = ("Segoe UI", "Segoe UI Emoji");
#[cfg(target_os = "linux")]
const FONTS: (&str, &str) = ("DejaVu Sans", "Noto Color Emoji");
#[cfg(target_os = "macos")]
const FONTS: (&str, &str) = ("Helvetica", "Apple Color Emoji");
#[cfg(target_os = "linux")]
// A ZWJ cluster exercises Linux fallback even when a monochrome emoji font is available.
const EMOJI: (&str, &str) = ("👩‍💻", FONTS.0);
#[cfg(not(target_os = "linux"))]
const EMOJI: (&str, &str) = ("😀", FONTS.1);

#[derive(Clone)]
struct NativeAssets {
    png: Arc<RenderImage>,
    gif: Arc<RenderImage>,
    #[cfg(target_os = "macos")]
    nv12: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
}

#[cfg(windows)]
#[test]
#[ignore = "requires the Windows DirectX renderer and locally built plugin runtime"]
fn sdk_canvas_matches_native_directx_pixels() -> Result<()> {
    let (surface, assets) = prepare_fixture()?;
    let result = Rc::new(std::cell::RefCell::new(None));
    let saved = Rc::clone(&result);
    let platform = Rc::new(gpui_windows::WindowsPlatform::new(false)?);
    gpui::Application::new_inaccessible(platform).run(move |cx| {
        *saved.borrow_mut() = Some(compare(cx, surface, assets, "plugin-canvas-parity"));
        cx.quit();
    });
    result
        .borrow_mut()
        .take()
        .context("GPUI did not run the parity scene")?
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a Linux WGPU adapter, the DejaVu and Noto Color Emoji fonts, and the plugin runtime"]
fn sdk_canvas_matches_native_wgpu_pixels() -> Result<()> {
    let text = linux_text(&*gpui_platform::current_platform(true))?;
    compare_offscreen(text, "plugin-canvas-parity-linux")
}

#[cfg(target_os = "linux")]
fn linux_text(platform: &dyn gpui::Platform) -> Result<Arc<dyn gpui::PlatformTextSystem>> {
    let text = platform.text_system();
    // A test machine can supply an unpacked font without changing its system font setup.
    if let Some(path) = std::env::var_os("NANOBUG_PARITY_EMOJI_FONT") {
        text.add_fonts(vec![std::borrow::Cow::Owned(fs::read(path)?)])?;
    }
    ensure!(
        text.all_font_names().iter().any(|name| name == FONTS.1),
        "Install Noto Color Emoji or set NANOBUG_PARITY_EMOJI_FONT"
    );
    Ok(text)
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires an Apple Metal device and locally built plugin runtime"]
fn sdk_canvas_matches_native_metal_pixels() -> Result<()> {
    compare_offscreen(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        "plugin-canvas-parity-macos",
    )
}

/// Use GPUI's production WGPU or Metal renderer and text shaper without opening a desktop window.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn compare_offscreen(text_system: Arc<dyn gpui::PlatformTextSystem>, evidence: &str) -> Result<()> {
    let (surface, assets) = prepare_fixture()?;
    gpui::HeadlessAppContext::with_platform(
        text_system,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    )
    .update(|cx| compare(cx, surface, assets, evidence))
}

fn prepare_fixture() -> Result<(Surface, NativeAssets)> {
    let temp = tempfile::tempdir()?;
    let pixels = RgbaImage::from_fn(8, 8, |x, y| {
        Rgba([40 + 23 * x as u8, 30 + 28 * y as u8, 210, 130 + 8 * x as u8])
    });
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(pixels).write_to(&mut png, image::ImageFormat::Png)?;
    let png = png.into_inner();
    let mut gif = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut gif);
        for color in [[201, 86, 49, 255], [31, 188, 126, 255]] {
            let pixels = RgbaImage::from_fn(8, 8, |x, y| {
                // A transparent border separates source pixels from adjacent atlas tiles.
                if x == 0 || y == 0 || x == 7 || y == 7 {
                    Rgba([0; 4])
                } else if (x + y) % 2 == 0 {
                    Rgba(color)
                } else {
                    Rgba([color[0] / 2, color[1], color[2], color[3]])
                }
            });
            encoder.encode_frame(Frame::from_parts(
                pixels,
                0,
                0,
                Delay::from_numer_denom_ms(80, 1),
            ))?;
        }
    }
    let surface = load_sdk_surface(temp.path(), &png, &gif)?;
    let decode = |format, bytes| {
        Image::from_bytes(format, bytes).to_image_data(SvgRenderer::new(Arc::new(())))
    };
    let assets = NativeAssets {
        png: decode(ImageFormat::Png, png)?,
        gif: decode(ImageFormat::Gif, gif)?,
        #[cfg(target_os = "macos")]
        nv12: apple::reference_buffer()?,
    };
    ensure!(
        assets.gif.frame_count() == 2,
        "The reference GIF must contain two frames"
    );
    let prepared = surface.drawing.as_ref().context("prepared images")?;
    let nanobug::plugins::drawing::Asset::Image(sdk_gif) = &prepared.assets["frames.gif"] else {
        anyhow::bail!("SDK GIF preparation did not retain a renderable image");
    };
    for frame in 0..2 {
        ensure!(
            sdk_gif.as_bytes(frame) == assets.gif.as_bytes(frame),
            "GIF frame bytes differ"
        );
    }
    // Compare frame selection and painting with the independently verified pixels
    // in the same atlas tiles. DirectX can sample adjacent pixels on unpadded tiles.
    let assets = NativeAssets {
        gif: Arc::clone(sdk_gif),
        ..assets
    };

    Ok((surface, assets))
}

/// Install the scene as an ordinary WASM package so assets and paths use production preparation.
fn load_sdk_surface(root: &Path, png: &[u8], gif: &[u8]) -> Result<Surface> {
    let scene = SCENE
        .replace("PARITY_TEXT_FALLBACK", FONTS.1)
        .replace("PARITY_TEXT_FONT", FONTS.0)
        .replace("PARITY_EMOJI_TEXT", EMOJI.0)
        .replace("PARITY_EMOJI_FONT", EMOJI.1);
    #[cfg(target_os = "macos")]
    let scene = apple::add_draw_to_scene(&scene)?;
    let output = scene
        .bytes()
        .map(|byte| format!("\\{byte:02x}"))
        .collect::<String>();
    let result = (16_u64 << 32) | scene.len() as u64;
    let wasm = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 20)
            (data (i32.const 16) "{output}")
            (func (export "nb_alloc") (param i32) (result i32) i32.const 65536)
            (func (export "nb_run") (param i32 i32) (result i64) i64.const {result})
            (func (export "nb_free") (param i32 i32)))"#
    ))?;
    let manifest = format!(
        "name='Canvas parity'\nid='canvas-parity'\nversion='1.0.0'\n[engines]\nnanobug='*'\n\
         [experiments]\nui={UI_CONTRACT:?}\n[[modules]]\nid='scene'\nwasm='main.wasm'\ninteractive=true\n"
    );
    let package = root.join("parity.zip");
    let mut zip = ZipWriter::new(File::create(&package)?);
    for (name, bytes) in [
        ("nanobug-plugin.toml", manifest.as_bytes()),
        ("main.wasm", &wasm),
        ("sample.png", png),
        ("frames.gif", gif),
        ("mark.svg", SVG),
    ] {
        zip.start_file(name, SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    #[cfg(target_os = "macos")]
    {
        zip.start_file(apple::ASSET, SimpleFileOptions::default())?;
        zip.write_all(&apple::packed_frame())?;
    }
    zip.finish()?;

    let project = root.join("project");
    fs::create_dir(&project)?;
    let (session, _updates) = Session::start(Options::take(
        &mut vec![
            "--plugin-dev-profile".into(),
            root.join("profile").into_os_string(),
        ],
        root,
    )?);
    session.projects(
        Some(1),
        vec![Project {
            id: 1,
            root: project,
            document: None,
            open_documents: Vec::new(),
        }],
    );
    session.send(Action::InspectZip(package))?;
    let token = wait_for(&session, |view| {
        view.review.as_ref().map(|review| review.token)
    })?;
    session.send(Action::Approve(token))?;
    wait_for(&session, |view| view.surfaces.values().next().cloned())
}

fn wait_for<T>(session: &Session, read: impl Fn(&View) -> Option<T>) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let view = session.view();
        if let Some(result) = read(&view) {
            return Ok(result);
        }
        ensure!(
            Instant::now() < deadline,
            "Canvas package did not publish its scene: {} · {:?} · {:?}",
            view.message,
            view.modules,
            view.logs
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct SceneView {
    surface: Surface,
    reference: Option<NativeAssets>,
    underlines: Rc<RefCell<Option<Vec<LineUnderlines>>>>,
}

impl Render for SceneView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let underlines = Rc::clone(&self.underlines);
        let content = if let Some(assets) = &self.reference {
            let assets = assets.clone();
            canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    let mut segments = Vec::new();
                    paint_reference(bounds, &assets, window, cx, &mut segments);
                    underlines.replace(Some(vec![LineUnderlines { tag: 17, segments }]));
                },
            )
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .into_any_element()
        } else {
            element_with_underlines(
                &self.surface,
                None,
                Some(move |lines, _: &Window, _: &mut App| {
                    underlines.replace(Some(lines));
                }),
            )
            .expect("the installed fixture must produce prepared drawing data")
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .into_any_element()
        };
        // Use GPUI's SVG element to construct the reference transform independently.
        let reference_svg = self.reference.as_ref().map(|_| {
            div()
                .absolute()
                .left(px(18.))
                .top(px(174.))
                .w(px(264.))
                .h(px(76.))
                .overflow_hidden()
                .child(
                    svg()
                        .data(SVG)
                        .absolute()
                        .left(px(4.))
                        .top(px(12.))
                        .w(px(52.))
                        .h(px(40.))
                        .text_color(rgba(0xf4c97aff))
                        .opacity(0.65)
                        .with_transformation(
                            Transformation::rotate(radians(0.45))
                                .with_scaling(size(1.15, 0.8))
                                .with_translation(point(px(7.), px(4.))),
                        ),
                )
        });
        div()
            .relative()
            .size_full()
            .bg(rgba(BACKGROUND))
            .child(content)
            .children(reference_svg)
    }
}

/// Use the same window for both captures so device scale and subpixel origin are equal.
fn compare(cx: &mut App, surface: Surface, assets: NativeAssets, evidence: &str) -> Result<()> {
    let reference = assets.clone();
    let underlines = Rc::new(RefCell::new(None));
    // GPUI's offscreen platform sizes its texture independently of the window's scale override.
    let extent = 2.;
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(rect(
                point(px(0.), px(0.)),
                [0., 0., WIDTH * extent, HEIGHT * extent],
            ))),
            titlebar: None,
            focus: false,
            show: false,
            ..WindowOptions::default()
        },
        |_, cx| {
            cx.new(|_| SceneView {
                surface,
                reference: Some(reference),
                underlines: Rc::clone(&underlines),
            })
        },
    )?;
    let capture = |cx: &mut App| -> Result<(RgbaImage, f32)> {
        cx.update_window(handle.into(), |_, window, cx| {
            verify_font_measurement(window)?;
            window.draw(cx).clear(cx);
            let scale = window.scale_factor();
            let (width, height) = (
                (WIDTH * scale).ceil() as u32,
                (HEIGHT * scale).ceil() as u32,
            );
            let scene = crop_scene(window.render_to_image()?, width, height)?;
            Ok((scene, scale))
        })?
    };
    let evidence = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test-output")
        .join(evidence);
    fs::create_dir_all(&evidence)?;
    let original_scale = handle.update(cx, |_, window, _| window.scale_factor())?;
    for (label, scale) in [
        ("", original_scale),
        ("100", 1.),
        ("125", 1.25),
        ("150", 1.5),
        ("200", 2.),
    ] {
        let select_reference = |cx: &mut App, reference: bool| {
            handle.update(cx, |view, window, cx| {
                window.set_scale_factor(scale);
                view.reference = reference.then(|| assets.clone());
                cx.notify();
            })
        };
        // Complete both sets of atlas allocations before comparing draws.
        for reference in [true, false] {
            select_reference(cx, reference)?;
            let _ = capture(cx)?;
        }
        select_reference(cx, true)?;
        let (native, scale) = capture(cx)?;
        let native_underlines = underlines.take().context("reference underline geometry")?;
        select_reference(cx, false)?;
        let (sdk, sdk_scale) = capture(cx)?;
        let sdk_underlines = underlines.take().context("SDK underline geometry")?;
        verify_underlines(&native_underlines, &sdk_underlines, scale)?;
        ensure!(scale == sdk_scale);
        let name = |file| {
            if label.is_empty() {
                format!("{file}.png")
            } else {
                format!("{file}-{label}.png")
            }
        };
        native.save(evidence.join(name("native")))?;
        sdk.save(evidence.join(name("sdk")))?;
        compare_pixels(&native, &sdk, scale, &evidence)?;
    }
    Ok(())
}

/// Check byte offsets and logical positions from the independent native reference and SDK paint.
fn verify_underlines(native: &[LineUnderlines], sdk: &[LineUnderlines], scale: f32) -> Result<()> {
    ensure!(native.len() == 1 && sdk.len() == 1 && native[0].tag == 17 && sdk[0].tag == 17);
    let native = &native[0].segments;
    let sdk = &sdk[0].segments;
    ensure!(
        native
            .iter()
            .map(|span| span.range)
            .eq([[0, 3], [3, 6], [6, 11]])
    );
    ensure!(
        serde_json::to_value(native)? == serde_json::to_value(sdk)?,
        "Underline geometry differs at {scale}x: {native:?} vs {sdk:?}"
    );
    ensure!(
        sdk.iter()
            .all(|span| span.origin[0] >= 147. && span.origin[1] > 74. && span.width > 0.)
    );
    println!("Native and SDK underline ranges, logical positions and styles match at {scale}x.");
    Ok(())
}

fn crop_scene(readback: RgbaImage, width: u32, height: u32) -> Result<RgbaImage> {
    ensure!(
        readback.width() >= width && readback.height() >= height,
        "The render target is smaller than the Canvas"
    );
    Ok(image::imageops::crop_imm(&readback, 0, 0, width, height).to_image())
}

fn compare_pixels(native: &RgbaImage, sdk: &RgbaImage, scale: f32, evidence: &Path) -> Result<()> {
    ensure!(
        native.dimensions() == sdk.dimensions(),
        "Canvas capture dimensions differ"
    );
    let changed = native
        .pixels()
        .filter(|pixel| **pixel != *native.get_pixel(0, 0))
        .count();
    ensure!(changed > 5_000, "Reference capture is missing its drawing");
    verify_assets_are_visible(native, scale)?;
    #[cfg(target_os = "macos")]
    apple::verify_surface_pixels(native, scale)?;
    let mut differences = 0;
    let mut outside_tolerance = 0;
    for (x, y, reference) in native.enumerate_pixels() {
        let actual = sdk.get_pixel(x, y);
        if reference == actual {
            continue;
        }
        differences += 1;
        let (x, y) = (x as f32 / scale, y as f32 / scale);
        // Translation before tessellation can round a transformed path edge differently.
        let path_edge = (22. ..114.).contains(&x) && (74. ..116.).contains(&y);
        // Native presentation can round SVG antialias coverage by one channel step.
        // Limit it to the affine image, where GPUI samples fractional texture coordinates.
        let affine_svg = (88. ..158.).contains(&x) && (225. ..250.).contains(&y);
        let delta = reference
            .0
            .iter()
            .zip(actual.0)
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap_or(0);
        if !(path_edge || affine_svg) || delta > 1 {
            outside_tolerance += 1;
        }
    }
    ensure!(
        outside_tolerance == 0 && differences <= 64,
        "{differences} pixels differ, {outside_tolerance} outside the transform-edge tolerance. Compare the images in {}",
        evidence.display()
    );
    println!(
        "Native and SDK RGBA compared at {} × {}, scale {scale}. {differences} transform-edge differences out of {} pixels, {changed} away from the background.",
        native.width(),
        native.height(),
        u64::from(native.width()) * u64::from(native.height())
    );
    Ok(())
}

/// Draw the reference with pinned GPUI calls and explicit Canvas-relative coordinates.
fn paint_reference(
    bounds: Bounds<Pixels>,
    assets: &NativeAssets,
    window: &mut Window,
    cx: &mut App,
    underlines: &mut Vec<UnderlineGeometry>,
) {
    let origin = bounds.origin;
    let shifted = origin + point(px(18.), px(14.));
    let clip = ContentMask {
        bounds: rect(shifted, [0., 0., 252., 148.]).intersect(&bounds),
    };
    window.with_content_mask(Some(clip), |window| {
        let card = rect(shifted, [4., 5., 112., 45.]);
        let radii = Corners {
            top_left: px(8.),
            top_right: px(3.),
            bottom_right: px(7.),
            bottom_left: px(4.),
        };
        window.paint_drop_shadows(
            card,
            radii,
            &[
                BoxShadow {
                    color: rgba(0x121525c8).into(),
                    offset: point(px(3.), px(5.)),
                    blur_radius: px(6.),
                    spread_radius: px(1.),
                    inset: false,
                },
                BoxShadow {
                    color: rgba(0x573873a0).into(),
                    offset: point(px(-2.), px(3.)),
                    blur_radius: px(3.),
                    spread_radius: px(0.),
                    inset: false,
                },
            ],
        );
        let gradient = linear_gradient(
            28.,
            linear_color_stop(rgba(0x3f6193ff), 0.),
            linear_color_stop(rgba(0xc689b7ff), 1.),
        )
        .color_space(ColorSpace::Oklab);
        window.paint_quad(quad(
            card,
            radii,
            gradient,
            Edges {
                top: px(1.),
                right: px(2.),
                bottom: px(3.),
                left: px(4.),
            },
            rgba(0xcddbf4b3),
            BorderStyle::Dashed,
        ));
        window.paint_inset_shadows(
            card,
            radii,
            &[BoxShadow {
                color: rgba(0x111b40b0).into(),
                offset: point(px(-2.), px(3.)),
                blur_radius: px(4.),
                spread_radius: px(0.),
                inset: true,
            }],
        );
        window.paint_quad(quad(
            rect(shifted, [133., 6., 138., 42.]),
            px(4.),
            pattern_slash(rgba(0xdca16dff), 3., 8.),
            px(0.),
            rgba(0),
            BorderStyle::Solid,
        ));

        let batch = origin + point(px(25.), px(76.));
        window.paint_layer(rect(batch, [0., 0., 220., 37.]), |window| {
            paint_path(batch, window);
            let mut opacity = Interactivity::default();
            opacity.base_style.opacity = Some(0.30);
            opacity.paint(None, None, clip.bounds, None, window, cx, |_, window, _| {
                window
                    .paint_image(
                        rect(batch, [102., 1., 72., 31.]),
                        rect(batch, [90., 0., 89., 34.]),
                        Corners::all(px(5.)),
                        Arc::clone(&assets.png),
                        0,
                        false,
                    )
                    .expect("paint native image");
            });
        });

        paint_text(shifted, window, cx);
        paint_local_style(shifted, origin, window, cx, underlines);
        window.paint_quad(fill(
            rect(origin, [236., 135., 50., 20.]),
            checkerboard(rgba(0xb8d0f0ff), 4.),
        ));
    });
    paint_polygon_strip(origin, window);
    paint_native_assets(origin, assets, window, cx);
    #[cfg(target_os = "macos")]
    apple::paint_reference(origin, &assets.nv12, window);
}

/// Compare the outline shorthand, a nonzero polygon fill, and the legacy stroke width.
fn paint_polygon_strip(origin: Point<Pixels>, window: &mut Window) {
    let origin = origin + point(px(16.), px(163.));
    let at = |x, y| origin + point(px(x), px(y));
    let clip = ContentMask {
        bounds: rect(origin, [0., -2., 270., 13.]),
    };
    window.with_content_mask(Some(clip), |window| {
        window.paint_quad(outline(
            rect(origin, [1., 0., 53., 8.]),
            rgba(0x99ed84ff),
            BorderStyle::Solid,
        ));
        let mut fill =
            PathBuilder::fill().with_style(PathStyle::Fill(gpui::FillOptions::non_zero()));
        fill.add_polygon(
            &[at(72., 0.), at(106., 0.), at(106., 8.), at(72., 8.)],
            true,
        );
        fill.add_polygon(&[at(83., 2.), at(96., 2.), at(96., 6.), at(83., 6.)], true);
        window.paint_path(
            fill.build().expect("build nonzero polygons"),
            checkerboard(rgba(0x5bc0beff), 2.),
        );
        let mut stroke = PathBuilder::stroke(px(1.5));
        stroke.move_to(at(126., 7.));
        stroke.line_to(at(139., 0.));
        stroke.line_to(at(163., 0.));
        window.paint_path(
            stroke.build().expect("build default stroke"),
            rgba(0xe1b07eff),
        );
    });
}

/// Check nested layers, inherited text style, and a standalone straight underline.
fn paint_local_style(
    origin: Point<Pixels>,
    canvas_origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
    underlines: &mut Vec<UnderlineGeometry>,
) {
    let at = origin + point(px(127.), px(56.));
    let clip = ContentMask {
        bounds: rect(at, [0., 0., 106., 40.]),
    };
    window.with_content_mask(Some(clip), |window| {
        window.paint_layer(rect(at, [0., 0., 118., 45.]), |window| {
            window.paint_quad(fill(rect(at, [-4., 0., 113., 29.]), rgba(0x398dceff)));
            window.paint_layer(rect(at, [4., 2., 102., 24.]), |window| {
                let mut opacity = Interactivity::default();
                opacity.base_style.opacity = Some(0.65);
                opacity.paint(
                    None,
                    None,
                    clip.bounds,
                    None,
                    window,
                    cx,
                    |_, window, cx| {
                        window.paint_quad(fill(rect(at, [4., 2., 102., 24.]), rgba(0xe13110ff)));
                        let style = gpui::TextStyleRefinement {
                            font_family: Some(FONTS.0.into()),
                            color: Some(rgba(0xd7f8ffff).into()),
                            underline: Some(UnderlineStyle {
                                thickness: px(1.1),
                                color: Some(rgba(0xd7f8ffff).into()),
                                wavy: true,
                            }),
                            ..Default::default()
                        };
                        window.with_text_style(Some(style), |window| {
                            let text = "Loc中Style";
                            let style = window.text_style();
                            let mut runs = [style.to_run(3), style.to_run(3), style.to_run(5)];
                            runs[1].underline = Some(UnderlineStyle {
                                thickness: px(2.),
                                color: Some(rgba(0xd7f8ffff).into()),
                                wavy: false,
                            });
                            let line =
                                window
                                    .text_system()
                                    .shape_line(text.into(), px(14.), &runs, None);
                            line.paint_with_underline_handler(
                                at + point(px(2.), px(4.)),
                                px(18.),
                                TextAlign::Right,
                                Some(px(93.)),
                                window,
                                cx,
                                |range, origin, width, style, window| {
                                    let local = origin - canvas_origin;
                                    let gpui::Rgba { r, g, b, a } = style.color.unwrap().into();
                                    underlines.push(UnderlineGeometry {
                                        range: [range.start, range.end],
                                        origin: [local.x.into(), local.y.into()],
                                        width: width.into(),
                                        thickness: style.thickness.into(),
                                        color: [r, g, b, a],
                                        wavy: style.wavy,
                                    });
                                    match (range.start, range.end) {
                                        (0, 3) => {}
                                        (6, 11) => {
                                            let factor = window.scale_factor();
                                            let exclusions = [
                                                (origin.x + px(5.5)).scale(factor)
                                                    ..(origin.x + px(10.25)).scale(factor),
                                                (origin.x + px(17.)).scale(factor)
                                                    ..(origin.x + px(23.)).scale(factor),
                                            ];
                                            window.paint_underline_with_exclusions(
                                                origin,
                                                width,
                                                &UnderlineStyle {
                                                    thickness: px(1.5),
                                                    ..*style
                                                },
                                                &exclusions,
                                            );
                                        }
                                        _ => window.paint_underline(origin, width, style),
                                    }
                                },
                            )
                            .expect("paint native inherited style");
                        });
                    },
                );
            });
            window.paint_underline(
                at + point(px(21.), px(31.)),
                px(72.),
                &UnderlineStyle {
                    color: Some(rgba(0x61e5dfff).into()),
                    thickness: px(2.),
                    wavy: false,
                },
            );
        });
    });
}

fn paint_native_assets(
    origin: Point<Pixels>,
    assets: &NativeAssets,
    window: &mut Window,
    cx: &mut App,
) {
    let origin = origin + point(px(18.), px(174.));
    let clip = ContentMask {
        bounds: rect(origin, [0., 0., 264., 76.]),
    };
    window.with_content_mask(Some(clip), |window| {
        let text = "A";
        let size = px(25.);
        let ink = rgba(0x95dcefff).into();
        let mut hash = DefaultHasher::new();
        text.hash(&mut hash);
        let glyph = window.text_system().shape_line_by_hash(
            hash.finish(),
            text.len(),
            size,
            &[TextRun {
                len: text.len(),
                font: font(FONTS.0),
                color: ink,
                ..TextRun::default()
            }],
            None,
            || text.into(),
        );
        let run = &glyph.runs[0];
        let glyph = &run.glyphs[0];
        assert!(!glyph.is_emoji);
        window
            .paint_glyph(
                origin + point(px(83.), px(44.)) + glyph.position,
                run.font_id,
                glyph.id,
                size,
                ink,
            )
            .expect("paint native single glyph");

        let (emoji, emoji_family) = EMOJI;
        let size = px(31.);
        let shaped = window.text_system().shape_line(
            emoji.into(),
            size,
            &[TextRun {
                len: emoji.len(),
                font: font(emoji_family),
                ..TextRun::default()
            }],
            None,
        );
        let run = &shaped.runs[0];
        let glyph = &run.glyphs[0];
        assert!(
            glyph.is_emoji,
            "The reference needs the {} color emoji font",
            FONTS.1
        );
        let mut opacity = Interactivity::default();
        opacity.base_style.opacity = Some(0.55);
        opacity.paint(None, None, clip.bounds, None, window, cx, |_, window, _| {
            window
                .paint_emoji(
                    origin + point(px(111.), px(44.)) + glyph.position,
                    run.font_id,
                    glyph.id,
                    size,
                )
                .expect("paint native emoji");
        });

        for (frame, x) in [(0, 156.), (1, 207.)] {
            let bounds = rect(origin, [x, 12., 38., 40.]);
            window
                .paint_image(
                    bounds,
                    bounds,
                    Corners::all(px(3.)),
                    Arc::clone(&assets.gif),
                    frame,
                    frame == 1,
                )
                .expect("paint native GIF frame");
        }

        let bounds = rect(origin, [74., 56., 52., 16.]);
        let bounds = Bounds::from_corners(
            window.pixel_snap_point(bounds.origin),
            window.pixel_snap_point(bounds.bottom_right()),
        );
        let scale = window.scale_factor();
        let center = bounds.center().scale(scale);
        let (x, y) = (center.x.0, center.y.0);
        // GPUI composes in device pixels, adding each matrix term from left to right.
        let matrix = TransformationMatrix {
            rotation_scale: [[0.9, 0.45], [-0.1, 1.]],
            translation: [
                ((x + scale) - 0.9 * x) - 0.45 * y,
                ((y - 2. * scale) + 0.1 * x) - y,
            ],
        };
        window
            .paint_svg(
                bounds,
                "native-parity-affine-svg".into(),
                Some(SVG),
                matrix,
                rgba(0x91b9e6ff).into(),
                cx,
            )
            .expect("paint native affine SVG");

        let mut path =
            PathBuilder::fill().with_style(PathStyle::Fill(gpui::FillOptions::even_odd()));
        let at = |x, y| origin + point(px(x), px(y));
        path.move_to(at(145., 55.));
        path.arc_to(point(px(19.), px(9.)), px(13.), false, true, at(182., 59.));
        path.curve_to(at(145., 55.), at(160., 84.));
        path.close();
        window.paint_path(
            path.build().expect("native arc and quadratic path"),
            linear_gradient(
                77.,
                linear_color_stop(rgba(0xdc786fff), 0.),
                linear_color_stop(rgba(0xeac88cff), 1.),
            ),
        );
    });
}

/// Rule out an equal pair of blank assets, and check that the selected GIF frames look different.
fn verify_assets_are_visible(image: &RgbaImage, scale: f32) -> Result<()> {
    let background = image.get_pixel(0, 0);
    let regions = [
        ("transformed SVG", [18., 174., 87., 247.]),
        ("single glyph", [100., 188., 123., 220.]),
        ("color emoji", [126., 183., 167., 226.]),
        ("affine SVG", [92., 227., 148., 250.]),
        ("arc and quadratic gradient", [162., 225., 202., 250.]),
    ];
    for (name, [left, top, right, bottom]) in regions {
        let pixels = (top * scale) as u32..(bottom * scale) as u32;
        let ink = pixels
            .flat_map(|y| {
                ((left * scale) as u32..(right * scale) as u32).map(move |x| image.get_pixel(x, y))
            })
            .filter(|pixel| *pixel != background)
            .count();
        ensure!(ink >= 20, "The reference has no visible {name}");
    }
    let first = image.get_pixel((192. * scale) as u32, (203. * scale) as u32);
    let second = image.get_pixel((242. * scale) as u32, (203. * scale) as u32);
    ensure!(
        first != background
            && second != background
            && first != second
            && first[0] != first[1]
            && second[0] == second[1]
            && second[1] == second[2],
        "The fixture needs a color first GIF frame and a grayscale second frame"
    );
    Ok(())
}

fn reference_font() -> gpui::Font {
    gpui::Font {
        weight: gpui::FontWeight::BOLD,
        style: gpui::FontStyle::Italic,
        features: gpui::FontFeatures(Arc::new(vec![("kern".into(), 1)])),
        fallbacks: Some(gpui::FontFallbacks::from_fonts(vec![FONTS.1.into()])),
        ..font(FONTS.0)
    }
}

/// Compare SDK metrics and grapheme positions with the native cached line-layout entry point.
fn verify_font_measurement(window: &Window) -> Result<()> {
    let text = "fi中文👩‍💻";
    let request = serde_json::from_value(serde_json::json!({
        "text": text, "size": 16., "font": FONTS.0,
        "font_options": {
            "weight": 700., "slant": "italic", "features": { "kern": 1 },
            "fallbacks": [FONTS.1],
        },
    }))?;
    let mut hash = DefaultHasher::new();
    text.hash(&mut hash);
    let line = window.text_system().shape_line_by_hash(
        hash.finish(),
        text.len(),
        px(16.),
        &[TextRun {
            len: text.len(),
            font: reference_font(),
            ..TextRun::default()
        }],
        None,
        || text.into(),
    );
    let positions = text.grapheme_indices(true).map(|(offset, _)| offset)
        .chain([text.len()])
        .map(|offset| serde_json::json!({"offset": offset, "x": f32::from(line.x_for_index(offset))}))
        .collect::<Vec<_>>();
    ensure!(
        super::measure(window, &request, Default::default())
            == serde_json::json!({
                "width": f32::from(line.width()), "ascent": f32::from(line.ascent),
                "descent": f32::from(line.descent), "positions": positions,
            }),
        "Native and SDK text measurements differ"
    );
    Ok(())
}

fn paint_path(origin: Point<Pixels>, window: &mut Window) {
    let mut path = PathBuilder::stroke(px(4.))
        .with_style(PathStyle::Stroke(
            StrokeOptions::default()
                .with_line_width(4.)
                .with_start_cap(lyon_path::LineCap::Round)
                .with_end_cap(lyon_path::LineCap::Square)
                .with_line_join(lyon_path::LineJoin::Bevel),
        ))
        .dash_array(&[px(6.), px(3.)]);
    // The transform includes the batch origin in the direct native version.
    path.transform(Transform::new(
        1.,
        0.125,
        -0.25,
        1.,
        f32::from(origin.x) + 4.,
        f32::from(origin.y) + 2.,
    ));
    path.move_to(point(px(0.), px(8.)));
    path.cubic_bezier_to(
        point(px(49.), px(10.)),
        point(px(13.), px(-4.)),
        point(px(33.), px(37.)),
    );
    path.line_to(point(px(77.), px(8.)));
    window.paint_path(
        path.build().expect("build native transformed path"),
        rgba(0x4ecbc5ff),
    );
}

fn paint_text(origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let text = "GPUI text\nWrap & paint";
    let underline = UnderlineStyle {
        thickness: px(1.5),
        color: Some(rgba(0xfabd81ff).into()),
        wavy: false,
    };
    let strike = StrikethroughStyle {
        thickness: px(1.2),
        color: Some(rgba(0xfd797dff).into()),
    };
    let lines = window
        .text_system()
        .shape_text(
            text.into(),
            px(16.),
            &[TextRun {
                len: text.len(),
                font: reference_font(),
                color: rgba(0xe9f4ffff).into(),
                background_color: Some(rgba(0x33445588).into()),
                underline: Some(underline),
                strikethrough: Some(strike),
            }],
            Some(px(98.)),
            Some(2),
        )
        .expect("shape native reference text");
    let mut at = origin + point(px(6.), px(102.));
    for line in lines {
        let alignment = Some(Bounds::new(at, size(px(118.), px(18.))));
        line.paint_background(at, px(18.), TextAlign::Center, alignment, window, cx)
            .expect("paint native text background");
        line.paint(at, px(18.), TextAlign::Center, alignment, window, cx)
            .expect("paint native text");
        at.y += px(18.) * (line.wrap_boundaries().len() + 1) as f32;
    }
    let at = origin + point(px(133.), px(119.));
    let scale = window.scale_factor();
    window.paint_underline_with_exclusions(
        at,
        px(86.),
        &UnderlineStyle {
            wavy: true,
            ..underline
        },
        &[
            (at.x + px(12.)).scale(scale)..(at.x + px(20.)).scale(scale),
            (at.x + px(44.)).scale(scale)..(at.x + px(50.)).scale(scale),
        ],
    );
    window.paint_strikethrough(origin + point(px(133.), px(130.)), px(86.), &strike);
}

fn rect(origin: Point<Pixels>, [x, y, width, height]: [f32; 4]) -> Bounds<Pixels> {
    Bounds::new(origin + point(px(x), px(y)), size(px(width), px(height)))
}
