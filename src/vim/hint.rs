//! Describe the next keys available in the modal parser's current state.

use super::{Mode, Pending, Vim};
use super::{folds::FOLD_OPERATOR, input::TRIM_YANK_OPERATOR};

/// Key label, description translation key and whether the key opens a subgroup.
pub type Continuation = (&'static str, &'static str, bool);

/// An incomplete command and the continuations to show beside it.
pub struct SequenceHint {
    /// Command keys entered so far, excluding counts and register selection.
    pub prefix: String,
    /// Translation key for the group name shown after the prefix.
    pub group: &'static str,
    /// The available next keys, their translation keys and whether they open a group.
    pub continuations: Vec<Continuation>,
}

/// Viewport commands shared with the Explorer hint.
pub const VIEW_CONTINUATIONS: &[Continuation] = &[
    ("z", "vim_hint.center", false),
    ("t", "vim_hint.top", false),
    ("b", "vim_hint.bottom", false),
];

/// The complete z group, with related completions kept on the same row.
const Z_CONTINUATIONS: &[Continuation] = &[
    ("a A", "vim_hint.fold_toggle", false),
    ("c C o O", "vim_hint.fold_open_close", false),
    ("d D E", "vim_hint.fold_delete", false),
    ("f", "vim_hint.fold_create", true),
    ("F", "vim_hint.fold_create_lines", false),
    ("i n N", "vim_hint.fold_enable", false),
    ("m M r R", "vim_hint.fold_levels", false),
    ("v x X", "vim_hint.fold_reveal", false),
    ("j k", "vim_hint.fold_motion", false),
    ("z t b", "vim_hint.view_place", false),
    ("↵ . -", "vim_hint.view_nonblank", false),
    ("+ ^", "vim_hint.view_page", false),
    ("h l H L ← →", "vim_hint.view_horizontal", false),
    ("s e", "vim_hint.view_column", false),
    ("0–9", "vim_hint.view_height", true),
    ("g G w W", "vim_hint.spelling_mark", false),
    ("u", "vim_hint.spelling_undo", true),
    ("=", "vim_hint.spelling_suggest", true),
    ("p P", "vim_hint.block_put", false),
    ("y", "vim_hint.block_yank", true),
];

/// Next keys for deleting an entry from a configured or session spelling list.
const SPELL_UNDO_CONTINUATIONS: &[Continuation] = &[
    ("g w", "vim_hint.spelling_remove_file", false),
    ("G W", "vim_hint.spelling_remove_session", false),
];

/// Motions implemented after an operator followed by z.
const Z_MOTION_CONTINUATIONS: &[Continuation] = &[("j k", "vim_hint.fold_motion", false)];

/// Next keys once the size or horizontal scroll distance is being entered.
const Z_NUMBER_CONTINUATIONS: &[Continuation] = &[
    ("0–9", "vim_hint.view_more_digits", true),
    ("Del", "vim_hint.view_delete_digit", true),
    ("↵", "vim_hint.view_apply_height", false),
    ("h l ← →", "vim_hint.view_horizontal", false),
];

/// Motions and text object prefixes that can follow any operator.
const OPERATOR_CONTINUATIONS: &[Continuation] = &[
    ("i", "vim_hint.inner_object", true),
    ("a", "vim_hint.around_object", true),
    ("g", "vim_hint.g_motion", true),
    ("[ ]", "vim_hint.method_motion", true),
    ("z", "vim_hint.fold_motion", true),
    ("v V Ctrl+v", "vim_hint.motion_shape", true),
    ("/ ? n N", "vim_hint.search_motion", false),
    ("h j k l 0 ^ $ - + _ |", "vim_hint.line_position", false),
    ("w W b B e E", "vim_hint.word_motion", false),
    ("G % H M L", "vim_hint.file_motion", false),
    ("( ) { }", "vim_hint.structural_motion", false),
    ("f F t T ; , ' `", "vim_hint.find_motion", false),
];

/// Keys for both inner and around text objects, with aliases on the same row.
const OBJECT_CONTINUATIONS: &[Continuation] = &[
    ("w", "vim_hint.word", false),
    ("W", "vim_hint.big_word", false),
    ("p", "vim_hint.paragraph", false),
    ("s", "vim_hint.sentence", false),
    ("t", "vim_hint.tag", false),
    ("\" ' `", "vim_hint.quotes", false),
    ("( ) b", "vim_hint.parentheses", false),
    ("{ } B", "vim_hint.braces", false),
    ("[ ]", "vim_hint.brackets", false),
    ("< >", "vim_hint.angles", false),
];

/// Group name and fixed completions for the entered motion, viewport or quit `prefix`.
fn prefix_group(prefix: char) -> Option<(&'static str, &'static [Continuation])> {
    Some(match prefix {
        'g' => (
            "vim_hint.goto_group",
            &[
                ("g", "vim_hint.first_line", false),
                ("_", "vim_hint.last_nonblank", false),
            ],
        ),
        'z' => ("vim_hint.fold_group", Z_CONTINUATIONS),
        'Z' => (
            "vim_hint.quit_group",
            &[
                ("Z", "vim_hint.save_exit", false),
                ("Q", "vim_hint.discard_exit", false),
            ],
        ),
        '[' => (
            "vim_hint.previous_group",
            super::bracket_commands::BRACKET_BACKWARD,
        ),
        ']' => (
            "vim_hint.next_group",
            super::bracket_commands::BRACKET_FORWARD,
        ),
        _ => return None,
    })
}

/// Write the visible command keys for `operator` into the entered `prefix`.
fn push_operator(prefix: &mut String, operator: super::Operator) {
    match operator.key {
        FOLD_OPERATOR => prefix.push_str("zf"),
        TRIM_YANK_OPERATOR => prefix.push_str("zy"),
        key => match super::g_commands::g_operator_name(key) {
            Some(name) => prefix.push_str(name),
            None => prefix.push(key),
        },
    }
    if let Some(force) = operator.force {
        prefix.push_str(match force {
            Mode::Visual => "v",
            Mode::VisualLine => "V",
            _ => " Ctrl+v",
        });
    }
}

impl Vim {
    /// Describe the currently pending command for the editor's hint panel.
    pub fn sequence_hint(&self) -> Option<SequenceHint> {
        let mut prefix = String::new();
        let (group, continuations) = match *self.pending.as_ref()? {
            Pending::Prefix(key, operator, _) => {
                if let Some(operator) = operator {
                    push_operator(&mut prefix, operator);
                }
                prefix.push(key);
                let (group, bindings) = prefix_group(key)?;
                let mut bindings = if key == 'z' && operator.is_some() {
                    Z_MOTION_CONTINUATIONS.to_vec()
                } else {
                    bindings.to_vec()
                };
                if key == 'z' && self.read_only && operator.is_none() {
                    bindings.retain(|(key, ..)| !matches!(*key, "=" | "p P"));
                }
                if key == 'g' {
                    bindings.extend_from_slice(super::g_commands::G_MOTIONS);
                    if operator.is_none() {
                        bindings.extend_from_slice(super::g_commands::G_CONTINUATIONS);
                        if self.read_only {
                            bindings.retain(|(key, _, _)| {
                                !matches!(
                                    *key,
                                    "u" | "U"
                                        | "~"
                                        | "?"
                                        | "q"
                                        | "w"
                                        | "J"
                                        | "I"
                                        | "i"
                                        | "r"
                                        | "R"
                                        | "p"
                                        | "P"
                                        | "&"
                                        | "+"
                                        | "-"
                                        | "t"
                                        | "T"
                                        | "Tab"
                                        | "x"
                                )
                            });
                        }
                    }
                }
                if operator.is_some() && matches!(key, '[' | ']') {
                    bindings.retain(|(keys, _, _)| super::bracket_commands::is_motion_hint(keys));
                } else if self.read_only && matches!(key, '[' | ']') {
                    bindings.retain(|(label, _, _)| {
                        !matches!(*label, "p P" | "p" | "P" | "Space" | "b" | "B" | "d" | "D")
                    });
                }
                (group, bindings)
            }
            Pending::Operator(operator) => {
                push_operator(&mut prefix, operator);
                let (key, group) = match operator.key {
                    'd' => ("d", "vim_hint.delete_group"),
                    'c' => ("c", "vim_hint.change_group"),
                    'y' => ("y", "vim_hint.yank_group"),
                    '<' => ("<", "vim_hint.outdent_group"),
                    '>' => (">", "vim_hint.indent_group"),
                    '=' => ("=", "vim_hint.reindent_group"),
                    '!' => ("!", "vim_hint.filter_group"),
                    FOLD_OPERATOR => ("zf", "vim_hint.fold_group"),
                    TRIM_YANK_OPERATOR => ("y", "vim_hint.yank_group"),
                    key => super::g_commands::g_operator_group(key)?,
                };
                let mut bindings = vec![(key, "vim_hint.current_line", false)];
                bindings.extend_from_slice(OPERATOR_CONTINUATIONS);
                (group, bindings)
            }
            Pending::Object {
                around, operator, ..
            } => {
                if let Some(operator) = operator {
                    push_operator(&mut prefix, operator);
                }
                prefix.push(if around { 'a' } else { 'i' });
                let group = if around {
                    "vim_hint.around_group"
                } else {
                    "vim_hint.inner_group"
                };
                (group, OBJECT_CONTINUATIONS.to_vec())
            }
            Pending::ZHeight { rows, .. } => {
                prefix = format!("z{rows}");
                ("vim_hint.scroll_group", Z_NUMBER_CONTINUATIONS.to_vec())
            }
            Pending::SpellUndo(_) => {
                prefix.push_str("zu");
                ("vim_hint.spelling_group", SPELL_UNDO_CONTINUATIONS.to_vec())
            }
            Pending::InsertControlG => {
                prefix.push_str("Ctrl+g");
                (
                    "vim_hint.insert_group",
                    vec![
                        ("u", "vim_hint.insert_undo_break", false),
                        ("U", "vim_hint.insert_undo_keep", false),
                        ("j k ↑ ↓", "vim_hint.insert_vertical", false),
                    ],
                )
            }
            Pending::ControlBackslash => {
                prefix.push_str("Ctrl+\\");
                (
                    "vim_hint.mode_group",
                    vec![("Ctrl+n", "vim_hint.normal_mode", false)],
                )
            }
            Pending::GArgument(ref argument) => return Some(argument.hint()),
            Pending::RegisterInput { modifier } => {
                prefix.push_str("Ctrl+r");
                if let Some(modifier) = modifier {
                    prefix.push_str(" Ctrl+");
                    prefix.push(modifier);
                }
                let mut bindings = vec![("a–z 0–9 \" % /", "vim_hint.register_value", false)];
                if modifier.is_none() {
                    bindings.push(("Ctrl+r Ctrl+o", "vim_hint.register_literal", true));
                }
                if self.mode.is_insert() {
                    if modifier.is_none() {
                        bindings.push(("Ctrl+p", "vim_hint.register_indent", true));
                    }
                } else {
                    bindings.push(("Ctrl+w/a/l/f/p", "vim_hint.register_cursor", false));
                }
                ("vim_hint.register_group", bindings)
            }
            Pending::Argument(..) | Pending::SpellSuggestions(_) => return None,
        };
        Some(SequenceHint {
            prefix,
            group,
            continuations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::Document, vim::Register};

    /// Every second key in the Neovim 0.12 z index is represented by the hint.
    #[test]
    fn z_hint_covers_the_reference_command_families() {
        let keys = Z_CONTINUATIONS
            .iter()
            .flat_map(|(keys, _, _)| keys.split_whitespace())
            .collect::<Vec<_>>();
        for key in "↵ . - + ^ = A C D E F G H L M N O P R W X a b c d e f g h i j k l m n o p r s t u v w x y z ← → 0–9".split_whitespace() {
            assert!(keys.contains(&key), "missing z continuation {key}");
        }
        for key in ["g", "w", "G", "W"] {
            assert!(
                SPELL_UNDO_CONTINUATIONS
                    .iter()
                    .any(|(keys, _, _)| keys.split_whitespace().any(|candidate| candidate == key))
            );
        }
    }

    /// Hints follow the parser through counts, motion prefixes and text objects.
    #[test]
    fn hints_follow_the_current_vim_sequence() {
        let mut vim = Vim::new(Document::new("alpha beta\ngamma"));
        let mut register = Register::default();
        vim.key("2", &mut register);
        assert!(vim.sequence_hint().is_none());
        vim.key("g", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "g");
        assert_eq!(hint.group, "vim_hint.goto_group");
        assert!(hint.continuations.starts_with(&[
            ("g", "vim_hint.first_line", false),
            ("_", "vim_hint.last_nonblank", false),
        ]));
        assert!(hint.continuations.iter().any(|row| row.0 == "d"));
        vim.key("Escape", &mut register);
        vim.key("z", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "z");
        assert_eq!(hint.group, "vim_hint.fold_group");
        vim.key("Escape", &mut register);
        vim.key("d", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "d");
        assert_eq!(hint.group, "vim_hint.delete_group");
        vim.key("g", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "dg");
        assert_eq!(hint.group, "vim_hint.goto_group");
        assert!(hint.continuations.iter().any(|row| row.0 == "d"));
        assert!(hint.continuations.iter().any(|row| row.0 == "g"));
        vim.key("Escape", &mut register);
        vim.text("vi", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "i");
        assert_eq!(hint.group, "vim_hint.inner_group");
        vim.key("w", &mut register);
        assert!(vim.sequence_hint().is_none());
        vim.key("f", &mut register);
        assert!(vim.sequence_hint().is_none());
    }
}
