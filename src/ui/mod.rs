//! Shared native UI support: text measurement, SVG raster caching, themes and input normalization.
//! Concrete grid windows and their retained renderer live in `grid`.

pub(crate) mod backdrop;
pub(crate) mod file_icons;
pub(crate) mod icons;
pub(crate) mod ime;
pub(crate) mod keys;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub(crate) mod native_atlas;
pub(crate) mod text;
pub(crate) mod theme;
pub(crate) mod window_geometry;
