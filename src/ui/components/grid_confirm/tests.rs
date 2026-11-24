use super::{ButtonAlign, GridConfirm};
use crate::ui::{mnemonic::Mnemonic, text::TextSpacing};
use gpui::{FontId, NoopTextSystem};
use std::sync::Arc;

/// The drawn range, accessible letter and click select one action, including clipped rows and gaps.
#[test]
fn localized_and_physical_buttons_share_their_visible_hit_targets() {
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 9.);
    for captions in [["&Yes", "&No"], ["是(&Y)", "否(&N)"]] {
        let choices =
            GridConfirm::new([true, false].into_iter().zip(captions.map(Mnemonic::parse)));
        let slots = choices.slots(4..40, ButtonAlign::Center, 2, &spacing);
        for (button, key) in slots.iter().zip(["y", "N"]) {
            assert_eq!(choices.key(key), Some(button.action));
            for col in button.columns.clone() {
                assert_eq!(GridConfirm::hit(&slots, col), choices.key(key));
            }
            assert_eq!(GridConfirm::hit(&slots, button.columns.end), None);
        }
        let clipped = choices.slots(2..4, ButtonAlign::Start, 2, &spacing);
        assert_eq!(clipped[0].columns, 2..4);
        assert_eq!(clipped[1].columns, 4..4);
        assert_eq!(GridConfirm::hit(&clipped, 4), None);
    }
    let confirm = GridConfirm::enter_escape();
    let buttons = confirm.slots(10..28, ButtonAlign::Start, 2, &spacing);
    assert_eq!(confirm.key("escape"), GridConfirm::hit(&buttons, 27));
    assert_eq!(confirm.key("enter"), GridConfirm::hit(&buttons, 10));
    assert_eq!(GridConfirm::hit(&buttons, 19), None);
}
