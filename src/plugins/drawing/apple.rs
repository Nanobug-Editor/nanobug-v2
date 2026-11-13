//! Prepare immutable IOSurface-backed buffers for GPUI's Apple paint_surface entry point.

use anyhow::{Context, Result, ensure};
use objc2_core_foundation::{CFDictionary, CFRetained, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPlaneCount,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVReturnSuccess,
};
use std::ptr::{self, NonNull};

/// The current scene owns this initialized surface. Painting retains it until GPUI releases the frame.
#[derive(Debug)]
pub struct AppleSurface {
    buffer: CFRetained<CVPixelBuffer>,
    pub(super) bytes: usize,
}

// SAFETY: pixel data is written only while locked, before publication. Once constructed, we
// only retain or release the buffer and let GPUI read it through Metal. CoreFoundation retains
// and releases are thread-safe and the surface is never CPU-mutated while shared.
unsafe impl Send for AppleSurface {}
unsafe impl Sync for AppleSurface {}

impl AppleSurface {
    /// Allocate a native full-range NV12 surface and fill it with the verified packed planes.
    pub(super) fn from_nv12([width, height]: [u16; 2], bytes: &[u8]) -> Result<Self> {
        let (width, height) = (usize::from(width), usize::from(height));
        ensure!(
            bytes.len() == width * (height + height / 2),
            "Incorrect NV12 plane length"
        );
        // This key makes the buffer usable by GPUI's Metal texture cache.
        let surface_properties = CFDictionary::<CFType, CFType>::empty();
        let attributes = CFDictionary::from_slices(
            &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
            &[&*surface_properties],
        );
        let mut buffer = ptr::null_mut();
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                Some(attributes.as_opaque()),
                NonNull::from(&mut buffer),
            )
        };
        ensure!(
            status == kCVReturnSuccess,
            "Could not allocate CoreVideo surface ({status})"
        );
        let buffer = unsafe {
            CFRetained::from_raw(NonNull::new(buffer).context("CoreVideo returned no buffer")?)
        };
        ensure!(
            CVPixelBufferGetPlaneCount(&buffer) == 2,
            "Expected two NV12 planes"
        );

        let flags = CVPixelBufferLockFlags::empty();
        let status = unsafe { CVPixelBufferLockBaseAddress(&buffer, flags) };
        ensure!(
            status == kCVReturnSuccess,
            "Could not lock CoreVideo surface ({status})"
        );
        // SAFETY: the buffer is exclusively owned here and the write lock was acquired above.
        let copied = unsafe { copy_planes(&buffer, width, height, bytes) };
        // Release the write lock even when CoreVideo returns an unexpected plane.
        let status = unsafe { CVPixelBufferUnlockBaseAddress(&buffer, flags) };
        let bytes = copied?;
        ensure!(
            status == kCVReturnSuccess,
            "Could not unlock CoreVideo surface ({status})"
        );
        Ok(Self { buffer, bytes })
    }

    /// Give the native frame its own read-only reference to the prepared surface.
    pub fn retain_for_paint(&self) -> CFRetained<CVPixelBuffer> {
        self.buffer.clone()
    }
}

/// Copy packed rows into each native stride.
///
/// The caller must own the buffer exclusively and hold its write lock. The source must
/// contain exactly one full frame at the supplied dimensions.
unsafe fn copy_planes(
    buffer: &CVPixelBuffer,
    width: usize,
    height: usize,
    mut bytes: &[u8],
) -> Result<usize> {
    let mut native_bytes = 0usize;
    for (plane, rows) in [height, height / 2].into_iter().enumerate() {
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
        ensure!(
            stride >= width && CVPixelBufferGetHeightOfPlane(buffer, plane) == rows,
            "Invalid CoreVideo plane layout"
        );
        native_bytes = native_bytes
            .checked_add(
                stride
                    .checked_mul(rows)
                    .context("CoreVideo plane is too large")?,
            )
            .context("CoreVideo surface is too large")?;
        let target = NonNull::new(CVPixelBufferGetBaseAddressOfPlane(buffer, plane).cast::<u8>())
            .context("CoreVideo returned an empty plane")?;
        for row in 0..rows {
            let (data, remaining) = bytes.split_at(width);
            // SAFETY: the source is a verified full plane and the checked native stride has room
            // for a packed row. The locked buffer is owned exclusively here.
            unsafe {
                ptr::copy_nonoverlapping(data.as_ptr(), target.as_ptr().add(row * stride), width)
            };
            bytes = remaining;
        }
    }
    Ok(native_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corevideo_surface_preserves_y_and_chroma_rows() -> Result<()> {
        let yuv = [1, 2, 3, 4, 5, 6, 7, 8, 101, 102, 103, 104];
        let surface = AppleSurface::from_nv12([4, 2], &yuv)?;
        let buffer = surface.retain_for_paint();
        let flags = CVPixelBufferLockFlags::ReadOnly;
        let status = unsafe { CVPixelBufferLockBaseAddress(&buffer, flags) };
        ensure!(
            status == kCVReturnSuccess,
            "Could not read test surface ({status})"
        );
        let y = CVPixelBufferGetBaseAddressOfPlane(&buffer, 0).cast::<u8>();
        let chroma = CVPixelBufferGetBaseAddressOfPlane(&buffer, 1).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, 0);
        // The checked constructor populated exactly these two Y rows and one interleaved UV row.
        let row_starts = unsafe { [*y, *y.add(stride), *chroma, *chroma.add(1)] };
        let status = unsafe { CVPixelBufferUnlockBaseAddress(&buffer, flags) };
        assert_eq!(status, kCVReturnSuccess);
        assert_eq!(row_starts, [1, 5, 101, 102]);
        Ok(())
    }
}
