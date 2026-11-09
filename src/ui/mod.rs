//! Shared native UI support: text measurement, SVG raster caching, themes and input normalization.
//! Concrete grid windows and their retained renderer live in `grid`.

pub(crate) mod backdrop;
pub(crate) mod icons;
pub(crate) mod ime;
pub(crate) mod keys;
pub(crate) mod text;
pub(crate) mod theme;
