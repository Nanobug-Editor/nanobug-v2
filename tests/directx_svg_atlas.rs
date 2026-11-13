//! Exercise explicit SVG eviction in the pinned native DirectX atlas.
#![cfg(target_os = "windows")]

use anyhow::{Context, Result, ensure};
use gpui::{
    AtlasKey, AtlasTextureKind, Bounds, DevicePixels, EmptyView, Platform, RenderSvgParams,
    WindowHandle, WindowId, WindowKind, WindowParams, px, size,
};
use gpui_windows::WindowsPlatform;
use std::{borrow::Cow, cell::Cell, rc::Rc};

/// An explicitly removed SVG releases its dedicated texture slot without touching adjacent keys.
#[test]
#[ignore = "requires a Windows desktop and a working DirectX device"]
fn directx_svg_atlas_reuses_removed_texture_slot() -> Result<()> {
    let platform = Rc::new(WindowsPlatform::new(false)?);
    let launched = Rc::clone(&platform);
    let result = Rc::new(Cell::new(None));
    let saved = Rc::clone(&result);
    platform.run(Box::new(move || {
        saved.set(Some(check_atlas(launched.as_ref())));
        launched.quit();
    }));
    result.take().context("The DirectX probe did not run")?
}

fn check_atlas(platform: &WindowsPlatform) -> Result<()> {
    let window = platform.open_window(
        WindowHandle::<EmptyView>::new(WindowId::from(1)).into(),
        WindowParams {
            bounds: Bounds::new(Default::default(), size(px(100.), px(100.))),
            titlebar: None,
            kind: WindowKind::Normal,
            is_movable: false,
            app_owns_titlebar_drag: false,
            is_resizable: false,
            is_minimizable: false,
            focus: false,
            show: false,
            icon: None,
            display_id: None,
            app_id: None,
            window_min_size: None,
        },
    )?;
    let atlas = window.sprite_atlas();
    // Each SVG fills a separate page in the pinned DirectX backend.
    let dimensions = size(DevicePixels(1024), DevicePixels(1024));
    let pixels = vec![127u8; 1024 * 1024];
    let key = |path: &'static str| {
        AtlasKey::Svg(RenderSvgParams {
            path: path.into(),
            size: dimensions,
        })
    };
    let builds = Cell::new(0);
    let tile = |key: &AtlasKey| {
        atlas
            .get_or_insert_with(key.clone(), &mut || {
                builds.set(builds.get() + 1);
                Ok(Some((dimensions, Cow::Borrowed(pixels.as_slice()))))
            })?
            .context("DirectX did not allocate the SVG tile")
    };

    let first_key = key("svg-atlas-probe-first");
    let other_key = key("svg-atlas-probe-other");
    let first = tile(&first_key)?;
    let other = tile(&other_key)?;
    ensure!(first.texture_id.kind == AtlasTextureKind::Monochrome);
    ensure!(first.texture_id != other.texture_id);
    ensure!(tile(&first_key)? == first && builds.get() == 2);

    atlas.remove(&first_key);
    let replacement = tile(&key("svg-atlas-probe-replacement"))?;
    ensure!(replacement.texture_id == first.texture_id);
    ensure!(tile(&other_key)? == other && builds.get() == 3);
    ensure!(tile(&first_key)?.texture_id != replacement.texture_id && builds.get() == 4);
    Ok(())
}
