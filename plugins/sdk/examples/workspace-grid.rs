//! A plugin panel using only the editor's character grid and host-owned input.

#[cfg(target_arch = "wasm32")]
use nanobug_plugin_sdk::{GridSurface, Input, Output, PluginEvent};

#[cfg(target_arch = "wasm32")]
nanobug_plugin_sdk::export_plugin!(handle);

#[cfg(target_arch = "wasm32")]
fn handle(input: Input) -> Output {
    let submitted = matches!(input.event, PluginEvent::Ui { action, .. } if action == "submit");
    let message = if submitted {
        "Submitted the current host draft"
    } else {
        "Type in the note field and press Enter"
    };
    Output {
        surface: Some(GridSurface {
            title: "Workspace Grid".into(),
            lines: vec![message.into()],
            input: Some(String::new()),
            canvas: None,
        }),
        ..Output::default()
    }
}
