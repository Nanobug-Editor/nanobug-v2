//! Run repeated SVG open and close cycles through a real platform's frame callbacks.

#[cfg(windows)]
#[path = "tests/windows_memory.rs"]
mod windows_memory;

use super::{NativeAtlases, finish_frame, mark_svg};
use anyhow::{Context as _, Result, ensure};
use gpui::{
    AnyWindowHandle, App, AppContext, Application, AsyncApp, Bounds, Context, IntoElement,
    ParentElement, Pixels, PlatformAtlas, Point, Render, RenderSvgParams, SharedString, Size,
    Styled, TitlebarOptions, TransformationMatrix, Window, WindowBounds, WindowHandle,
    WindowOptions, canvas, div, point, px, rgba, size,
};
#[cfg(not(windows))]
use serde_json::Value;
use serde_json::json;
use std::{
    collections::HashSet,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><rect width="20" height="20"/></svg>"#;
const DEFAULT_CYCLES: usize = 120;
const DEFAULT_KEYS: usize = 24;
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(5);

struct Scene {
    cycle: usize,
    keys: usize,
    plugin_open: bool,
    shared_visible: bool,
}

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let Self {
            cycle,
            keys,
            plugin_open,
            shared_visible,
        } = *self;
        div().size_full().child(
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    if shared_visible {
                        let position = point(px(11.25), px(8.));
                        let path = SharedString::from("nanobug-plugin-svg:soak-keeper");
                        paint_svg(window, cx, position, 48., &path);
                    }
                    if plugin_open {
                        for index in 0..keys {
                            let position = point(
                                px((index % 6) as f32 * 64. + 23.25),
                                px((index / 6) as f32 * 52. + 13.),
                            );
                            let path = format!("nanobug-plugin-svg:soak-{cycle}-{index}").into();
                            paint_svg(window, cx, position, 80., &path);
                        }
                    }
                    finish_frame(window, cx);
                },
            )
            .size_full(),
        )
    }
}

fn paint_svg(window: &mut Window, cx: &App, origin: Point<Pixels>, edge: f32, path: &SharedString) {
    let bounds = Bounds::new(origin, size(px(edge), px(edge)));
    if window
        .paint_svg(
            bounds,
            path.clone(),
            Some(SVG),
            TransformationMatrix::unit(),
            rgba(0xffffffff).into(),
            cx,
        )
        .is_ok()
    {
        mark_svg(window, cx, bounds, path);
    }
}

/// Cocoa must run this probe on the main thread, so the macOS runner has its own main.
#[cfg(not(target_os = "macos"))]
#[test]
#[ignore = "requires a visible native desktop window and a working graphics adapter"]
fn native_svg_atlas_frame_retirement_soak() -> Result<()> {
    run()
}

pub fn run() -> Result<()> {
    let cycles = test_setting("NANOBUG_ATLAS_CYCLES", DEFAULT_CYCLES, 1, 1800)?;
    let keys = test_setting("NANOBUG_ATLAS_KEYS", DEFAULT_KEYS, 2, 256)?;
    let sample_delay =
        Duration::from_millis(test_setting("NANOBUG_ATLAS_SAMPLE_MS", 100, 0, 4000)? as u64);
    let (platform, atlases) = NativeAtlases::wrap(gpui_platform::current_platform(false));
    let output = Rc::new(std::cell::Cell::new(None));
    let saved = Rc::clone(&output);
    Application::new_inaccessible(platform).run(move |cx| {
        atlases.install(cx);
        // Calloop resets its stop signal after launch. Quit from the running event loop.
        cx.spawn(async move |cx| {
            saved.set(Some(soak(cycles, keys, sample_delay, cx).await));
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    output.take().context("the native GPUI probe did not run")?
}

fn test_setting(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let value = match std::env::var(name) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("{name} must be an integer"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    ensure!((min..=max).contains(&value), "{name} must be {min}–{max}");
    Ok(value)
}

fn probe_window(title: &'static str, dimensions: Size<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(48.), px(48.)),
            dimensions,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        focus: false,
        inactive_frame_interval: None,
        ..Default::default()
    }
}

async fn soak(cycles: usize, keys: usize, sample_delay: Duration, cx: &mut AsyncApp) -> Result<()> {
    let began = Instant::now();
    // Keep the app running after closing the visible probe on platforms that quit with
    // their last window. The spare window does not paint any SVGs.
    let _spare = cx.open_window(
        WindowOptions {
            show: false,
            ..probe_window("Nanobug atlas sampler", size(px(100.), px(100.)))
        },
        |_, cx| {
            cx.new(|_| Scene {
                cycle: 0,
                keys: 0,
                plugin_open: false,
                shared_visible: false,
            })
        },
    )?;
    let handle = cx.open_window(
        probe_window("Nanobug SVG atlas probe", size(px(440.), px(300.))),
        |_, cx| {
            cx.new(|_| Scene {
                cycle: 0,
                keys,
                plugin_open: false,
                shared_visible: true,
            })
        },
    )?;
    let id = AnyWindowHandle::from(handle).window_id();
    let gpu = handle.update(cx, |_, window, _| window.gpu_specs())?;
    #[cfg(not(target_os = "macos"))]
    ensure!(gpu.is_some(), "this probe needs an actual renderer");
    let compositor = cx.update(|cx| cx.compositor_name());
    let (atlas, initial) = await_atlas(handle, 1, cx).await?;
    let keeper = initial.into_iter().next().context("shared SVG key")?;
    let original_tile = lookup(&atlas, &keeper)?.context("shared SVG tile")?;

    println!(
        "ATLAS_BEGIN {}",
        json!({
            "pid": std::process::id(),
            "compositor": compositor,
            "device": gpu.as_ref().map(|specs| &specs.device_name),
            "driver": gpu.as_ref().map(|specs| &specs.driver_name),
            "software_emulated": gpu.as_ref().map(|specs| specs.is_software_emulated),
            "cycles": cycles,
            "unique_svg_per_cycle": keys,
        })
    );
    let mut memory = MemorySampler::new()?;
    cx.background_executor().timer(sample_delay).await;
    emit_sample(0, "baseline", 1, &mut memory, began)?;

    let mut unique_texture_slots = HashSet::new();
    let mut max_texture_slots = 0;
    for cycle in 1..=cycles {
        handle.update(cx, |scene, _, cx| {
            scene.cycle = cycle;
            scene.plugin_open = true;
            cx.notify();
        })?;
        let (active_atlas, displayed) = await_atlas(handle, keys + 1, cx).await?;
        ensure!(Arc::ptr_eq(&atlas, &active_atlas), "native atlas changed");
        ensure!(lookup(&atlas, &keeper)? == Some(original_tile));
        let mut textures = HashSet::new();
        let opened = displayed
            .into_iter()
            .filter(|key| key != &keeper)
            .collect::<Vec<_>>();
        for key in &opened {
            ensure!(
                key.path
                    .starts_with(&format!("nanobug-plugin-svg:soak-{cycle}-"))
            );
            let tile = lookup(&atlas, key)?.context("a newly painted SVG is missing")?;
            textures.insert(tile.texture_id);
        }
        max_texture_slots = max_texture_slots.max(textures.len());
        unique_texture_slots.extend(textures);

        let sample = cycle == 1 || cycle == cycles || cycle % 10 == 0;
        if sample {
            cx.background_executor().timer(sample_delay).await;
            emit_sample(cycle, "open", keys + 1, &mut memory, began)?;
        }

        handle.update(cx, |scene, _, cx| {
            scene.plugin_open = false;
            cx.notify();
        })?;
        await_atlas(handle, 1, cx).await?;
        for key in &opened {
            ensure!(
                lookup(&atlas, key)?.is_none(),
                "an old SVG key survived close"
            );
        }
        ensure!(lookup(&atlas, &keeper)? == Some(original_tile));
        if sample {
            cx.background_executor().timer(sample_delay).await;
            emit_sample(cycle, "closed", 1, &mut memory, began)?;
        }
    }

    // Now the last visible owner goes away. Its native entry should retire as well.
    handle.update(cx, |scene, _, cx| {
        scene.shared_visible = false;
        cx.notify();
    })?;
    await_atlas(handle, 0, cx).await?;
    ensure!(
        lookup(&atlas, &keeper)?.is_none(),
        "the last SVG key survived close"
    );
    drop(atlas);
    handle.update(cx, |_, window, _| window.remove_window())?;
    ensure!(
        !cx.update(|cx| cx.global::<NativeAtlases>().0.borrow().contains_key(&id)),
        "closing the native window left an atlas owner"
    );
    cx.background_executor().timer(sample_delay).await;
    emit_sample(cycles, "window_closed", 0, &mut memory, began)?;

    // The renderer can hold several submitted frames, but cycling unique keys
    // should reuse atlas texture slots instead of allocating a new set per panel.
    ensure!(
        unique_texture_slots.len() <= max_texture_slots * 4 + 1,
        "{} texture slots used for at most {max_texture_slots} per open panel",
        unique_texture_slots.len(),
    );
    println!(
        "ATLAS_RESULT {}",
        json!({
            "elapsed_ms": began.elapsed().as_millis(),
            "cycles": cycles,
            "svg_keys_created_and_retired": cycles * keys,
            "max_concurrent_plugin_texture_slots": max_texture_slots,
            "distinct_plugin_texture_slots": unique_texture_slots.len(),
        })
    );
    Ok(())
}

/// Wait until both paint and the real platform frame callback have run.
async fn await_atlas(
    handle: WindowHandle<Scene>,
    expected: usize,
    cx: &mut AsyncApp,
) -> Result<(Arc<dyn PlatformAtlas>, Vec<RenderSvgParams>)> {
    let id = AnyWindowHandle::from(handle).window_id();
    let start = Instant::now();
    loop {
        if let Some(result) = cx.update(|cx| {
            let windows = cx.global::<NativeAtlases>().0.borrow();
            let state = windows.get(&id)?;
            if state.displayed.len() == expected && !state.retirement_queued {
                Some((
                    state.atlas.upgrade()?,
                    state.displayed.iter().cloned().collect(),
                ))
            } else {
                None
            }
        }) {
            return Ok(result);
        }
        ensure!(
            start.elapsed() < OBSERVE_TIMEOUT,
            "real frame callback did not settle the SVG atlas to {expected} keys"
        );
        cx.background_executor()
            .timer(Duration::from_millis(4))
            .await;
    }
}

fn lookup(
    atlas: &Arc<dyn PlatformAtlas>,
    key: &RenderSvgParams,
) -> Result<Option<gpui::AtlasTile>> {
    atlas.get_or_insert_with(key.clone().into(), &mut || Ok(None))
}

fn emit_sample(
    cycle: usize,
    phase: &str,
    atlas_keys: usize,
    memory: &mut MemorySampler,
    began: Instant,
) -> Result<()> {
    let data = memory.sample()?;
    println!(
        "ATLAS_SAMPLE {}",
        json!({
            "cycle": cycle,
            "phase": phase,
            "atlas_keys": atlas_keys,
            "elapsed_ms": began.elapsed().as_millis(),
            "memory": data,
        })
    );
    Ok(())
}

#[cfg(windows)]
use windows_memory::MemorySampler;

#[cfg(target_os = "macos")]
struct MemorySampler;

#[cfg(target_os = "macos")]
impl MemorySampler {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    fn sample(&mut self) -> Result<Value> {
        let pid = std::process::id().to_string();
        let output = std::process::Command::new("/bin/ps")
            .args(["-p", &pid, "-o", "rss="])
            .output()?;
        ensure!(
            output.status.success(),
            "ps could not sample resident memory"
        );
        let kib: u64 = std::str::from_utf8(&output.stdout)?.trim().parse()?;
        Ok(json!({"resident_bytes": kib * 1024}))
    }
}

#[cfg(target_os = "linux")]
struct MemorySampler;

#[cfg(target_os = "linux")]
impl MemorySampler {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    fn sample(&mut self) -> Result<Value> {
        let text = std::fs::read_to_string("/proc/self/smaps_rollup")?;
        let bytes = |label: &str| -> Option<u64> {
            let data = text.lines().find_map(|line| line.strip_prefix(label))?;
            data.split_ascii_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|kib| kib * 1024)
        };
        let private = bytes("Private_Clean:").context("private clean memory")?
            + bytes("Private_Dirty:").context("private dirty memory")?;
        Ok(json!({
            "private_resident_bytes": private,
            "resident_bytes": bytes("Rss:"),
            "drm_clients": drm_clients()?,
        }))
    }
}

/// Keep each driver's original per-client counters. A shared DRM fd can be duplicated.
#[cfg(target_os = "linux")]
fn drm_clients() -> Result<Vec<Value>> {
    use std::collections::BTreeMap;
    let mut clients = BTreeMap::new();
    for entry in std::fs::read_dir("/proc/self/fdinfo")? {
        let Ok(text) = std::fs::read_to_string(entry?.path()) else {
            continue;
        };
        let fields = text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key, value.trim()))
            .collect::<BTreeMap<_, _>>();
        let Some(client) = fields.get("drm-client-id") else {
            continue;
        };
        let device = fields
            .get("drm-pdev")
            .or_else(|| fields.get("drm-driver"))
            .copied()
            .unwrap_or("unknown");
        let memory = fields
            .iter()
            .filter(|(key, _)| {
                ["drm-resident-", "drm-memory-", "drm-total-", "drm-shared-"]
                    .iter()
                    .any(|prefix| key.starts_with(prefix))
            })
            .collect::<BTreeMap<_, _>>();
        clients.insert(
            (device.to_owned(), (*client).to_owned()),
            json!({"device": device, "client_id": client, "memory": memory}),
        );
    }
    Ok(clients.into_values().collect())
}
