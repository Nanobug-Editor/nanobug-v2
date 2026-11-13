//! Demonstrate full Canvas snapshots, incremental edits, and a deliberately missed baseline.

#[cfg(target_arch = "wasm32")]
use nanobug_plugin_sdk::{
    GridSurface, Input, Output, PluginEvent,
    canvas::{Canvas, Change, Delta, Draw, Fill},
};

#[cfg(target_arch = "wasm32")]
nanobug_plugin_sdk::export_plugin!(handle);

#[cfg(target_arch = "wasm32")]
fn handle(input: Input) -> Output {
    let color = 0x6184a5ff;
    let mut baseline = color;
    if let PluginEvent::Ui { action, argument } = &input.event {
        match action.as_str() {
            "inspect" => {
                return Output {
                    message: serde_json::to_string(&input.canvas_sync)
                        .expect("encode Canvas baseline"),
                    ..Output::default()
                };
            }
            "delta" | "gap" | "bad_asset" => {
                let Some(sync) = input.canvas_sync else {
                    return Output::default();
                };
                let Some(base) = sync.sequence else {
                    return Output::default();
                };
                let stale = |name| argument.get(name).and_then(serde_json::Value::as_u64);
                let base = stale("base").unwrap_or(base);
                let sequence = if action == "gap" { base + 2 } else { base + 1 };
                let changes = if action == "bad_asset" {
                    vec![Change::Splice {
                        start: 0,
                        remove: 1,
                        draw: vec![Draw::Image {
                            bounds: [14., 12., 80., 26.],
                            image_bounds: None,
                            asset: "unavailable.png".into(),
                            frame: 0,
                            corners: [0.; 4],
                            grayscale: false,
                        }],
                    }]
                } else {
                    vec![Change::Splice {
                        start: 0,
                        remove: 1,
                        draw: vec![quad(stale("color").unwrap_or(0x799ccdff) as u32)],
                    }]
                };
                return Output {
                    canvas_delta: Some(Delta {
                        generation: stale("generation").unwrap_or(sync.generation),
                        viewport: stale("viewport").or(sync.viewport),
                        epoch: stale("epoch").unwrap_or(sync.epoch),
                        base,
                        sequence,
                        changes,
                    }),
                    ..Output::default()
                };
            }
            "complete" => baseline = argument.as_u64().unwrap_or(u64::from(color)) as u32,
            _ => return Output::default(),
        }
    } else if matches!(input.event, PluginEvent::CanvasResync) {
        // Return a complete scene. A new host epoch prevents delayed operations
        // from matching this new baseline even when the sequence is reused.
        baseline = 0x7e5d98ff;
    } else if !matches!(input.event, PluginEvent::Open | PluginEvent::Layout) {
        return Output::default();
    }
    Output {
        surface: Some(GridSurface {
            title: "Canvas deltas".into(),
            input: Some(String::new()),
            canvas: Some(Canvas {
                viewport: input.viewport.map(|viewport| viewport.id),
                sequence: Some(1),
                draw: vec![quad(baseline)],
                input: Some([14., 46., 160., 22.]),
                ..Canvas::default()
            }),
            ..GridSurface::default()
        }),
        ..Output::default()
    }
}

#[cfg(target_arch = "wasm32")]
fn quad(color: u32) -> Draw {
    Draw::Quad {
        bounds: [14., 12., 80., 26.],
        fill: Fill::Solid { color },
        corners: [5.; 4],
        borders: [0.; 4],
        border: 0,
        dashed: false,
    }
}
