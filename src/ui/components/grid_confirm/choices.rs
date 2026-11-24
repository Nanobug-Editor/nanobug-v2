use super::{ButtonAlign, ConfirmButton};
use crate::ui::{
    components::grid_button_row::GridButtonRow, mnemonic::Mnemonic, text::TextSpacing,
};
use std::ops::Range;

/// Presents the available answers to a confirmation through one shared button row.
pub(crate) struct GridConfirm<A>(GridButtonRow<A>);

impl<A: Copy> GridConfirm<A> {
    pub(crate) fn new(choices: impl IntoIterator<Item = (A, Mnemonic)>) -> Self {
        Self(GridButtonRow::new(choices))
    }
    pub(crate) fn key(&self, key: &str) -> Option<A> {
        self.0.key(key)
    }
    pub(crate) fn slots(
        &self,
        columns: Range<usize>,
        align: ButtonAlign,
        gap: usize,
        spacing: &TextSpacing,
    ) -> Vec<ConfirmButton<A>> {
        self.0.slots(columns, align, gap, spacing)
    }
    pub(crate) fn hit(slots: &[ConfirmButton<A>], column: usize) -> Option<A> {
        GridButtonRow::hit(slots, column)
    }
}

impl GridConfirm<bool> {
    pub(crate) fn enter_escape() -> Self {
        Self(GridButtonRow::enter_escape())
    }
}
