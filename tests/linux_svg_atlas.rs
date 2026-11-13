//! Run the native Linux SVG soak on the main thread with `-- --run`.

#[cfg(target_os = "linux")]
#[path = "../src/ui/native_atlas.rs"]
mod native_atlas;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--run") {
        // WSLg's D3D12 OpenGL stack may fault on worker-thread teardown.
        #[cfg(target_os = "linux")]
        return native_atlas::tests::run();
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("this native SVG probe requires a Linux desktop");
    }
    Ok(())
}
