//! Compare the SDK's packed NV12 asset with a CoreVideo buffer painted directly by GPUI.

use super::rect;
use anyhow::{Context as _, Result, ensure};
use gpui::{ContentMask, Pixels, Point, Window, fill, rgba};
use image::{Rgba, RgbaImage};
use objc2_core_foundation::{CFDictionary, CFRetained, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVReturnSuccess,
};
use std::ptr::{self, NonNull};

pub(super) const ASSET: &str = "checker.nv12";
const WIDTH: usize = 16;
const HEIGHT: usize = 8;
const SURFACE: [f32; 4] = [20., 267., 104., 48.];
const CLIP: [f32; 4] = [29., 273., 72., 34.];
const OVERLAY: [f32; 4] = [90., 293., 12., 7.];
const OVERLAY_COLOR: u32 = 0x33ef70ff;

/// Build different luma and chroma values on each side of both planes.
fn component(plane: usize, x: usize, y: usize) -> u8 {
    if plane == 0 {
        (if x < WIDTH / 2 { 65 } else { 185 }) + if y < HEIGHT / 2 { 0 } else { 22 }
    } else {
        match (x < WIDTH / 2, x % 2 == 0) {
            (true, true) => 88,
            (true, false) => 163,
            (false, true) => 162,
            (false, false) => 93,
        }
    }
}

pub(super) fn packed_frame() -> Vec<u8> {
    let mut bytes = Vec::new();
    for (plane, rows) in [HEIGHT, HEIGHT / 2].into_iter().enumerate() {
        for y in 0..rows {
            bytes.extend((0..WIDTH).map(|x| component(plane, x, y)));
        }
    }
    bytes
}

pub(super) fn add_draw_to_scene(scene: &str) -> Result<String> {
    let mut scene: serde_json::Value = serde_json::from_str(scene)?;
    let draw = scene["surface"]["canvas"]["draw"]
        .as_array_mut()
        .context("Canvas fixture draw array")?;
    draw.push(serde_json::json!({
        "op": "group",
        "clip": CLIP,
        "draw": [
            { "op": "apple_surface", "bounds": SURFACE, "asset": ASSET, "dimensions": [WIDTH, HEIGHT] },
            { "op": "quad", "bounds": OVERLAY, "fill": { "fill": "solid", "color": OVERLAY_COLOR } }
        ]
    }));
    Ok(serde_json::to_string(&scene)?)
}

/// Allocate the native reference independently and write each pixel at its CoreVideo stride.
pub(super) fn reference_buffer() -> Result<CFRetained<CVPixelBuffer>> {
    let surface = CFDictionary::<CFType, CFType>::empty();
    // SAFETY: CoreVideo exports a process-lifetime key and takes a dictionary of CF references.
    let properties = CFDictionary::from_slices(
        &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
        &[&*surface],
    );
    let mut raw = ptr::null_mut();
    // SAFETY: the out pointer is valid and receives a retained pixel buffer on success.
    let created = unsafe {
        CVPixelBufferCreate(
            None,
            WIDTH,
            HEIGHT,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            Some(properties.as_opaque()),
            NonNull::from(&mut raw),
        )
    };
    ensure!(
        created == kCVReturnSuccess,
        "Create reference CoreVideo buffer: {created}"
    );
    // SAFETY: a nonnull successful result transfers one retain to CFRetained.
    let buffer =
        unsafe { CFRetained::from_raw(NonNull::new(raw).context("Reference pixel buffer")?) };
    let flags = CVPixelBufferLockFlags::empty();
    // SAFETY: this reference buffer is exclusively owned and the lock is released below.
    let locked = unsafe { CVPixelBufferLockBaseAddress(&buffer, flags) };
    ensure!(
        locked == kCVReturnSuccess,
        "Lock reference CoreVideo buffer: {locked}"
    );
    let written = (|| -> Result<()> {
        for (plane, rows) in [HEIGHT, HEIGHT / 2].into_iter().enumerate() {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            ensure!(
                !base.is_null()
                    && stride >= WIDTH
                    && CVPixelBufferGetHeightOfPlane(&buffer, plane) == rows,
                "Invalid reference plane"
            );
            for y in 0..rows {
                for x in 0..WIDTH {
                    // SAFETY: every write is inside the locked CoreVideo plane.
                    unsafe { base.add(y * stride + x).write(component(plane, x, y)) };
                }
            }
        }
        Ok(())
    })();
    // SAFETY: CoreVideo requires a matching unlock even if writing the plane returned an error.
    let unlocked = unsafe { CVPixelBufferUnlockBaseAddress(&buffer, flags) };
    written?;
    ensure!(
        unlocked == kCVReturnSuccess,
        "Unlock reference CoreVideo buffer: {unlocked}"
    );
    Ok(buffer)
}

pub(super) fn paint_reference(
    origin: Point<Pixels>,
    buffer: &CFRetained<CVPixelBuffer>,
    window: &mut Window,
) {
    let mask = ContentMask {
        bounds: rect(origin, CLIP),
    };
    window.with_content_mask(Some(mask), |window| {
        window.paint_surface(rect(origin, SURFACE), buffer.clone());
        window.paint_quad(fill(rect(origin, OVERLAY), rgba(OVERLAY_COLOR)));
    });
}

/// Require visible color and luma changes, clipping and a later primitive painted over NV12.
pub(super) fn verify_surface_pixels(image: &RgbaImage, scale: f32) -> Result<()> {
    let at = |x: f32, y: f32| *image.get_pixel((x * scale) as u32, (y * scale) as u32);
    let background = at(0., 0.);
    let left = at(40., 281.);
    let right = at(91., 281.);
    let bottom = at(40., 300.);
    ensure!(
        left != background && right != background && left != right && bottom != left,
        "The native NV12 luma and chroma bands were not painted"
    );
    ensure!(
        at(22., 285.) == background && at(110., 285.) == background,
        "The native NV12 surface escaped its content mask"
    );
    ensure!(
        at(95., 297.) == Rgba(OVERLAY_COLOR.to_be_bytes()),
        "The later quad did not paint over the native NV12 surface"
    );
    Ok(())
}
