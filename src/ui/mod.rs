//! Native drawing, text measurement and the independently animated cursor view.
//! These views render derived state and do not own editable document copies.

pub(crate) mod backdrop;
mod cursor;
pub(crate) mod grid;
pub(crate) mod text;
pub(crate) mod theme;

pub(crate) use cursor::Cursor;
