//! Measure confirmation choices once for drawing, pointer hits and keyboard actions.

mod choices;
pub(crate) use crate::ui::components::grid_button_row::{ButtonAlign, ButtonSlot as ConfirmButton};
pub(crate) use choices::GridConfirm;

#[cfg(test)]
mod tests;
