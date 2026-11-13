# GPUI Apple renderer

Source is the published `gpui-pre-apple` 0.3.10 crate, Apache-2.0, from Zed revision `08cbb88a152c3751a43e853425a70e9f631a5918`. The published crate's SHA-256 is `e97cd068d8134b4e073321b946491760aa1e4511e486084a92e73b9174bff78a`. [Published source](https://crates.io/crates/gpui-pre-apple/0.3.10)

Nanobug removes unused plugin SVG keys after replacing the scene that used them. Metal's CPU texture upload does not synchronize against prior GPU reads, so the backend keeps a removed tile's allocation until its last possible GPU use completes. The existing command buffer completion handler releases retired tiles after the callbacks for all preceding submissions have arrived. Each renderer owns one atlas and submits to one queue. The render thread does not wait for a completion. The same retirement protects image tiles returned by GPUI's image cache. [Metal command queues](https://developer.apple.com/library/archive/documentation/Conceptual/MetalProgrammingGuide/Cmd-Submiss/Cmd-Submiss.html) · [Metal texture uploads](https://developer.apple.com/documentation/metal/mtltexture/replace%28region%3Amipmaplevel%3Aslice%3Awithbytes%3Abytesperrow%3Abytesperimage%3A%29)

The build script selects the Apple target from Cargo's environment. `--features gpui_platform/runtime_shaders` lets a non-Apple host cross-check Apple types and generate the bundled shader source. Native Apple builds with Xcode still compile the Metal library by default.

On a Mac desktop with Metal and Xcode, run the visible window probe from the Nanobug checkout.

```sh
NANOBUG_ATLAS_CYCLES=240 cargo test --release --locked --test macos_svg_atlas -- --run
```

The `ATLAS_FENCE` line confirms that retired allocations remain unavailable until all earlier completions arrive. `ATLAS_RESULT` reports completed cycles and distinct texture slots. `ATLAS_SAMPLE` reports process resident memory. Record the full output and the Mac model, macOS version, and display scale. For GPU allocation bytes, record the probe process with the Metal Resource Events track in Instruments. [Apple's Metal memory guide](https://developer.apple.com/documentation/xcode/analyzing-the-memory-usage-of-your-metal-app)
