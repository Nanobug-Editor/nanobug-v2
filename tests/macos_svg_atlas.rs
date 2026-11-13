//! Run with `cargo test --release --test macos_svg_atlas -- --run` on a Mac desktop.

#[cfg(target_os = "macos")]
#[path = "../src/ui/native_atlas.rs"]
mod native_atlas;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--run") {
        #[cfg(target_os = "macos")]
        {
            gpui_apple::metal_renderer::MetalRenderer::test_atlas_retirement_order()?;
            return native_atlas::tests::run();
        }
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("use the native SVG atlas ignored test on Windows or Linux");
    }
    Ok(())
}
