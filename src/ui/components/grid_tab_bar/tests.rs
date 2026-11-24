use super::{GridTabBar, TabAlign, TabGrowth, TabOptions, TitleTabs};
use crate::ui::{components::grid_window::title_hit_columns, text::TextSpacing};
use gpui::{FontId, NoopTextSystem};
use std::sync::Arc;

/// Blank cells belong to the adjacent title while the divider and the rest of the rule stay inert.
#[test]
fn title_targets_include_padding_and_preserve_the_divider() {
    let spacing = TextSpacing::new(Arc::new(NoopTextSystem::new()), FontId(0), 16., 10.);
    assert_eq!(title_hit_columns(6..9), 5..10);
    assert_eq!(title_hit_columns(6..6), 6..6);

    let tabs = TitleTabs::new(3..30, ["One", "", "Three"], false, &spacing);
    assert_eq!(tabs.hit_columns(0), 2..7);
    assert!(tabs.hit_columns(1).is_empty());
    assert_eq!(tabs.hit_columns(2), 8..15);
    assert_eq!(tabs.tab_at(2), Some(0));
    assert_eq!(tabs.tab_at(6), Some(0));
    assert_eq!(tabs.tab_at(7), None);
    assert_eq!(tabs.tab_at(8), Some(2));
    assert_eq!(tabs.tab_at(14), Some(2));
    assert_eq!(tabs.tab_at(15), None);

    let compact = TitleTabs::new(3..9, ["One", "Five", "Hidden"], true, &spacing);
    assert_eq!(compact.hit_columns(0), 2..6);
    assert_eq!(compact.tab_at(6), None);
    assert_eq!(compact.hit_columns(1), 7..10);
    assert!(compact.hit_columns(2).is_empty());
}

#[test]
fn tight_strips_leave_a_clickable_active_tab_before_reserving_arrows() {
    let widths = [3, 45, 5, 20, 3];
    for budget in 1..64 {
        for active in 0..widths.len() {
            let bar = GridTabBar::fit(
                widths.len(),
                |i| widths[i],
                active,
                5..5 + budget,
                TabOptions {
                    gap: 3,
                    hit_padding: 1,
                    minimum: 1,
                    growth: TabGrowth::Narrowest,
                    align: TabAlign::Start,
                },
            );
            assert!(bar.visible.contains(&active));
            let selected = bar
                .slots
                .iter()
                .find(|s| s.target() == Some(active))
                .unwrap();
            assert!(!selected.columns.is_empty() && selected.hit.contains(&selected.columns.start));
            let mut end = 0;
            for slot in bar.slots {
                assert!(slot.columns.start >= 5 && slot.columns.end <= 5 + budget);
                if slot.target().is_some() {
                    assert!(slot.hit.start >= end);
                    end = slot.hit.end;
                }
            }
        }
    }
}
