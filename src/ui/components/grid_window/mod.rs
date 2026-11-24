//! A bordered grid window with fixed rows and a scrollable body.

mod layout;
mod titles;

pub(crate) use layout::GridWindow;
pub(crate) use titles::title_hit_columns;

#[cfg(test)]
mod tests;
