//! Cursor configuration, lifecycle, spring motion and silhouette geometry.
//! Callers drive the model with timestamps; drawing and frame scheduling stay in the UI.

mod lifecycle;
mod motion;
mod shape;

pub use lifecycle::{Binding, CursorConfig, Kind, Stage, Target, Wave};
pub use motion::{CursorMotion, Effect, Geometry, Spring, Vec2};
pub use shape::{CubicSegment, FLUID_SEGMENTS};
