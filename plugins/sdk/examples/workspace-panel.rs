//! Example plugin for a native panel with editable, host-owned input and a Canvas background.

#[cfg(target_arch = "wasm32")]
use nanobug_plugin_sdk::{
    GridSurface, Input, Output, PluginEvent,
    canvas::{
        Canvas, CursorStyle, Draw, Fill, Hit, Interaction, KeyBinding, KeyModifiers, LineCap,
        LineJoin, PathStyle, PointerRegion, Segment, TextInput,
    },
};

#[cfg(target_arch = "wasm32")]
nanobug_plugin_sdk::export_plugin!(handle);

#[cfg(target_arch = "wasm32")]
fn handle(input: Input) -> Output {
    let caption = match &input.event {
        PluginEvent::Ui { action, .. } => match action.as_str() {
            "heading" => "Heading clicked",
            "pressed" => "Pointer pressed",
            "released" => "Pointer released",
            "moved" => "Pointer moved",
            "scrolled" => "Canvas scrolled",
            "panel-key" => "Canvas key pressed",
            "painted" => return Output::default(),
            _ => "",
        },
        PluginEvent::Diagnose => {
            return Output {
                message: "Workspace panel is ready".into(),
                ..Output::default()
            };
        }
        _ => "",
    };
    let width = input
        .viewport
        .map_or(260., |viewport| viewport.width.clamp(200., 700.));
    let appearance = input.viewport.and_then(|viewport| viewport.appearance);
    let language = appearance.map_or(Default::default(), |appearance| appearance.language);
    let notes = language.text("Workspace notes", Some("工作区笔记"));
    let accent = appearance.map_or(0x92cddaff, |appearance| appearance.colors.accent);
    let font_size = appearance.map_or(16., |appearance| appearance.font_size);
    let line_height = (font_size * 1.25).ceil();
    let underline = 22. + line_height + 4.;
    let input_top = underline + 6.;
    let input_height = line_height + 4.;
    let heading = [16., 22., width - 32., line_height];
    let next_frame = (input.viewport.is_some()
        && matches!(
            input.event,
            PluginEvent::Open | PluginEvent::Layout | PluginEvent::DocumentChanged
        ))
    .then(|| "painted".into());
    Output {
        surface: Some(GridSurface {
            title: "Workspace panel".into(),
            lines: Vec::new(),
            input: Some(String::new()),
            canvas: Some(Canvas {
                viewport: input.viewport.map(|viewport| viewport.id),
                sequence: None,
                input: None,
                text_input: Some(TextInput {
                    bounds: [18., input_top, width - 36., input_height],
                    font: None,
                    font_options: None,
                    size: font_size,
                    color: appearance.map(|appearance| appearance.colors.text),
                }),
                hits: vec![Hit {
                    bounds: heading,
                    action: "heading".into(),
                }],
                interaction: Interaction {
                    pointer: vec![PointerRegion {
                        bounds: heading,
                        cursor: Some(CursorStyle::PointingHand),
                        mouse_down: Some("pressed".into()),
                        mouse_up: Some("released".into()),
                        mouse_move: Some("moved".into()),
                        wheel: Some("scrolled".into()),
                    }],
                    keys: vec![KeyBinding {
                        key: "f2".into(),
                        modifiers: KeyModifiers::default(),
                        action: "panel-key".into(),
                    }],
                    next_frame,
                },
                draw: vec![
                    Draw::Quad {
                        bounds: [14., 18., width - 28., input_top + input_height - 10.],
                        fill: Fill::Linear {
                            angle: 90.,
                            from: (0x263651ff, 0.),
                            to: (0x344c59ff, 1.),
                            oklab: true,
                        },
                        corners: [5.; 4],
                        borders: [1.; 4],
                        border: 0x8098adff,
                        dashed: false,
                    },
                    Draw::Text {
                        origin: [20., 22.],
                        text: format!("{notes} 你好 👋"),
                        size: font_size,
                        line_height,
                        wrap: None,
                        runs: Vec::new(),
                        layout: None,
                    },
                    Draw::Group {
                        clip: Some([16., underline - 5., width - 32., 10.]),
                        offset: None,
                        absolute_offset: None,
                        layer: None,
                        opacity: Some(0.65),
                        draw: vec![Draw::Path {
                            segments: vec![
                                Segment::Move {
                                    to: [20., underline],
                                },
                                Segment::Line {
                                    to: [width - 20., underline],
                                },
                            ],
                            fill: Fill::Solid { color: accent },
                            stroke: None,
                            style: Some(PathStyle::Stroke {
                                width: 2.,
                                start_cap: LineCap::Round,
                                end_cap: LineCap::Round,
                                join: LineJoin::Round,
                                miter_limit: None,
                                tolerance: None,
                            }),
                            dash: vec![5., 3.],
                            transform: None,
                        }],
                    },
                    Draw::Text {
                        origin: [20., input_top + input_height + 12.],
                        text: language.text(caption, None).into(),
                        size: 14.,
                        line_height: 18.,
                        wrap: None,
                        runs: Vec::new(),
                        layout: None,
                    },
                ],
            }),
        }),
        ..Output::default()
    }
}
