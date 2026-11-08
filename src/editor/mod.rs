//! Editable documents, modal commands and native key normalization.
//! The application owns windows and views; this module owns text and editing state.

mod document;
mod keys;
mod vim;

pub use document::{Document, display_width};
pub use keys::normal_character;
pub use vim::{Action, Mode, Register, Vim};
