//! Shortcut navigation, filtering and popup geometry regressions.

use super::{
    HelpCommand,
    list::{HelpList, Scope},
    view::{HELP_BORDER_GAP, help_outline},
};
use crate::grid::render::Metrics;
use gpui::{Bounds, KeyDownEvent, Keystroke, point, px, size};
use nanobug::{
    animation::scroll::ScrollInput,
    i18n::{self, Locale},
};

/// A search term present in the save/open and refresh descriptions of the current locale.
fn file_query() -> &'static str {
    match i18n::locale() {
        Locale::English => "FILE",
        Locale::SimplifiedChinese => "文件",
    }
}

/// Press a binding in `list` with a `height`-row viewport at `now` and apply its scroll intent.
fn press(list: &mut HelpList, key: &'static str, height: usize, now: f64) -> HelpCommand {
    let event = KeyDownEvent {
        keystroke: Keystroke::parse(key).unwrap(),
        is_held: false,
        prefer_character_input: false,
    };
    let command = list.key(&event, height, now, false);
    match command {
        HelpCommand::Update(input) => {
            list.track(input, height, now);
        }
        HelpCommand::FinishFilter { cancel } => {
            list.finish_filter(cancel, now);
            list.track(ScrollInput::Advance, height, now);
        }
        _ => {}
    }
    command
}

/// The window and global lists stay separate, and only descriptions match a query.
#[test]
fn switch_scopes_and_filter_descriptions_from_the_keyboard() {
    let mut list = HelpList::new(true, true);
    let title = match i18n::locale() {
        Locale::English => "Explorer shortcuts",
        Locale::SimplifiedChinese => "文件管理器快捷键",
    };
    assert_eq!(list.window_title(), title);
    assert!(list.scope() == Scope::Window);
    assert!(!list.rows.iter().any(|(key, _)| *key == "F5"));
    press(&mut list, "l", 10, 0.);
    assert!(list.scope() == Scope::Global);
    assert!(list.rows.iter().any(|(key, _)| *key == "F5"));

    press(&mut list, "/", 10, 0.);
    assert!(list.is_filtering());
    assert!(matches!(press(&mut list, "j", 10, 0.), HelpCommand::Text));
    list.commit_filter("F5", 0.);
    assert_eq!(list.len(), 0);
    list.commit_filter("🙂", 0.);
    for _ in 0..3 {
        press(&mut list, "backspace", 10, 0.);
    }
    list.commit_filter(file_query(), 0.);
    assert!(
        list.rows
            .contains(&("F5", i18n::text("global_shortcuts.refresh")))
    );
    press(&mut list, "enter", 10, 0.);
    assert!(!list.is_filtering());
    press(&mut list, "h", 10, 0.);
    assert!(
        list.rows
            .contains(&("l / Enter", i18n::text("manager_shortcuts.open")))
    );
    let window_matches = list.rows.clone();

    press(&mut list, "/", 10, 0.);
    list.commit_filter("不存在", 0.);
    assert_eq!(list.len(), 0);
    press(&mut list, "escape", 10, 0.);
    assert_eq!(list.rows, window_matches);
    assert!(matches!(
        press(&mut list, "escape", 10, 0.),
        HelpCommand::Close
    ));
}

/// Help closes on a single q or Escape while browsing; the filter handles those keys locally.
#[test]
fn help_close_keys_respect_filter_input() {
    let mut list = HelpList::new(true, true);
    assert!(matches!(
        press(&mut list, "shift-x", 10, 0.),
        HelpCommand::Ignore
    ));
    assert!(matches!(press(&mut list, "q", 10, 0.), HelpCommand::Close));
    assert!(matches!(
        press(&mut list, "escape", 10, 0.),
        HelpCommand::Close
    ));
    let held_q = KeyDownEvent {
        keystroke: Keystroke::parse("q").unwrap(),
        is_held: true,
        prefer_character_input: false,
    };
    assert!(matches!(
        list.key(&held_q, 10, 0., false),
        HelpCommand::Ignore
    ));

    press(&mut list, "/", 10, 0.);
    assert!(matches!(press(&mut list, "q", 10, 0.), HelpCommand::Text));
    assert!(matches!(
        press(&mut list, "escape", 10, 0.),
        HelpCommand::FinishFilter { cancel: true }
    ));
    assert!(!list.is_filtering());
}

/// Opening search over the selected bottom row preserves the rows above it.
#[test]
fn search_covers_the_last_visible_row_without_scrolling() {
    let mut list = HelpList::new(false, true);
    let height = list.len();
    press(&mut list, "shift-g", height, 0.);
    assert_eq!(list.selected, height - 1);

    press(&mut list, "/", height - 1, 0.);
    assert_eq!(list.scroll.target(), 0);
    assert_eq!(list.scroll.position(), 0.);
    press(&mut list, "escape", height, 0.);
    assert_eq!(list.selected, height - 1);
    assert_eq!(list.scroll.position(), 0.);
}

/// Page keys move the selection by the visible row count, then reveal it smoothly.
#[test]
fn page_keys_use_the_editors_row_distance() {
    let mut list = HelpList::new(false, true);
    let height = 8;
    list.track(ScrollInput::Advance, height, 0.);
    press(&mut list, "pagedown", height, 0.);
    assert_eq!(list.selected, 8);
    assert_eq!(list.scroll.target(), 4);
    list.track(ScrollInput::Advance, height, 0.03);
    assert!(list.scroll.fraction() > 0.);
    list.track(ScrollInput::Advance, height, 0.5);
    assert_eq!(list.scroll.position(), 4.);
    press(&mut list, "pageup", height, 0.5);
    assert_eq!(list.selected, 0);
}

/// Read-only Normal jumps work, while editing commands and retired chords stay local and inert.
#[test]
fn normal_jumps_are_available_without_editor_modes() {
    let mut list = HelpList::new(false, true);
    let last = list.len() - 1;
    press(&mut list, "shift-g", 10, 0.);
    assert_eq!(list.selected, last);
    press(&mut list, "g", 10, 0.);
    assert_eq!(list.selected, last);
    press(&mut list, "g", 10, 0.);
    assert_eq!(list.selected, 0);
    for key in ["1", "2", "shift-g"] {
        press(&mut list, key, 10, 0.);
    }
    assert_eq!(list.selected, 11);
    for key in ["3", "g", "g"] {
        press(&mut list, key, 10, 0.);
    }
    assert_eq!(list.selected, 2);
    for key in [
        "ctrl-h", "ctrl-l", "ctrl-j", "ctrl-k", "i", "v", "shift-v", ":", "d", "x",
    ] {
        assert!(matches!(press(&mut list, key, 10, 0.), HelpCommand::Ignore));
    }
    assert!(list.scope() == Scope::Window);
    assert_eq!(list.selected, 2);
    assert_eq!(list.len(), last + 1);
    assert!(!list.is_filtering());

    press(&mut list, "/", 10, 0.);
    list.commit_filter(file_query(), 0.);
    press(&mut list, "enter", 10, 0.);
    assert!(list.len() > 1);
    press(&mut list, "shift-n", 10, 0.);
    assert_eq!(list.selected, list.len() - 1);
    press(&mut list, "n", 10, 0.);
    assert_eq!(list.selected, 0);
}

/// Numeric row commands move the caret by the whole count, revealing the fourth row at either edge.
#[test]
fn counted_motion_reveals_the_selection_with_editor_margins() {
    let mut list = HelpList::new(false, true);
    let height = 10;
    list.track(ScrollInput::Advance, height, 0.);
    for key in ["1", "2", "j"] {
        press(&mut list, key, height, 0.);
    }
    assert_eq!(list.selected, 12);
    assert_eq!(list.scroll.target() + height - 4, list.selected);
    list.track(ScrollInput::Advance, height, 0.5);
    for key in ["7", "k"] {
        press(&mut list, key, height, 0.5);
    }
    assert_eq!(list.selected, 5);
    assert_eq!(list.scroll.target() + 3, list.selected);
}

/// The visible character edges are equally inset despite unequal cell dimensions.
#[test]
fn help_outline_is_even_around_the_character_border() {
    let metrics = Metrics {
        cw: 10.,
        lh: 21.,
        font_size: 16.,
        origin: point(px(0.), px(0.)),
    };
    let grid = Bounds::new(point(px(100.), px(50.)), size(px(640.), px(462.)));
    let outline = help_outline(grid, metrics);
    assert_eq!(
        grid.left() + px(metrics.cw / 2.) - outline.left(),
        px(HELP_BORDER_GAP)
    );
    assert_eq!(
        outline.right() - (grid.right() - px(metrics.cw / 2.)),
        px(HELP_BORDER_GAP)
    );
    assert_eq!(
        grid.top() + px(metrics.lh / 2.) - outline.top(),
        px(HELP_BORDER_GAP)
    );
    assert_eq!(
        outline.bottom() - (grid.bottom() - px(metrics.lh / 2.)),
        px(HELP_BORDER_GAP)
    );
}
