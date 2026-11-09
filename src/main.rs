//! Native GPUI entry point. Application composition lives in `app`.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod grid;
mod ui;

/// Start the native application with its command-line project or file.
fn main() {
    app::run();
}
