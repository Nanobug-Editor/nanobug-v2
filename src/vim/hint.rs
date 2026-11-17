//! Describe the next keys available in the modal parser's current state.

use super::{Mode, Pending, Vim};
use super::{folds::FOLD_OPERATOR, input::TRIM_YANK_OPERATOR};

/// Key label, description translation key and whether the key opens a subgroup.
pub type Continuation = (&'static str, &'static str, bool);

/// An incomplete command and the continuations to show beside it.
pub struct SequenceHint {
    /// Command keys entered so far, excluding counts and register selection.
    pub prefix: String,
    /// Translation key for the current group's title.
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
                    if operator.is_some() {
                        bindings.push(("c", "vim_hint.comment_object", false));
                    } else {
                        bindings.extend_from_slice(super::g_commands::G_CONTINUATIONS);
                        if self.help_buffer && self.mode == Mode::Normal {
                            bindings.push(("=", "vim_g.help_run", true));
                        }
                        if self.mode.is_visual() && !self.read_only {
                            bindings.extend([
                                ("Ctrl+a", "vim_g.increment_sequence", false),
                                ("Ctrl+x", "vim_g.decrement_sequence", false),
                            ]);
                        }
                        if self.mode.is_visual()
                            && let Some(comment) =
                                bindings.iter_mut().find(|(key, _, _)| *key == "c")
                        {
                            comment.2 = false;
                        }
                        if self.mode.is_visual()
                            && let Some(custom) =
                                bindings.iter_mut().find(|(key, _, _)| *key == "@")
                        {
                            custom.2 = false;
                        }
                        if self.read_only {
                            bindings.retain(|(key, _, _)| {
                                !matches!(
                                    *key,
                                    "u" | "U"
                                        | "~"
                                        | "?"
                                        | "q"
                                        | "w"
                                        | "c"
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
                } else if matches!(key, '[' | ']') {
                    bindings.retain(|(label, _, _)| match *label {
                        "Space" | "a" | "A" | "b" | "B" | "d" | "D" => {
                            self.mode == Mode::Normal && !self.read_only
                        }
                        "p P" | "p" | "P" | "MiddleMouse" => !self.read_only,
                        _ => true,
                    });
                }
                if self.mode.is_visual() && operator.is_none() {
                    if key == '[' {
                        bindings.push(("n", "vim_hint.syntax_previous", false));
                    } else if key == ']' {
                        bindings.push(("n", "vim_hint.syntax_next", false));
                    }
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
                let mut bindings = OBJECT_CONTINUATIONS.to_vec();
                bindings.push((
                    "n",
                    if around {
                        "vim_hint.syntax_parent"
                    } else {
                        "vim_hint.syntax_child"
                    },
                    false,
                ));
                (group, bindings)
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
            Pending::InsertCompletion => {
                prefix.push_str("Ctrl+x");
                (
                    "vim_hint.insert_group",
                    vec![
                        ("Ctrl+o Ctrl+u", "vim_hint.insert_omni_completion", false),
                        ("Ctrl+f", "vim_hint.insert_file_completion", false),
                        ("Ctrl+l", "vim_hint.insert_line_completion", false),
                        ("Ctrl+n Ctrl+p", "vim_hint.insert_word_completion", false),
                        ("Ctrl+i", "vim_hint.insert_identifier_completion", false),
                        ("Ctrl+d", "vim_hint.insert_definition_completion", false),
                        ("Ctrl+k", "vim_hint.insert_dictionary_completion", false),
                        ("Ctrl+t", "vim_hint.insert_thesaurus_completion", false),
                        ("Ctrl+r", "vim_hint.insert_register_completion", false),
                        ("Ctrl+v Ctrl+q", "vim_hint.insert_ex_completion", false),
                        ("Ctrl+s s", "vim_hint.insert_spelling_completion", false),
                        ("Ctrl+z", "vim_hint.insert_completion_cancel", false),
                        ("Ctrl+e Ctrl+y", "vim_hint.insert_scroll", false),
                    ],
                )
            }
            Pending::InsertArgument(argument) => {
                use super::insert_argument::{Argument, Target};

                let target = match argument {
                    Argument::Digraph { target, .. }
                    | Argument::Literal { target, .. }
                    | Argument::Number { target, .. } => target,
                };
                if let Target::Find(_, _, Some(operator)) = target {
                    push_operator(&mut prefix, operator);
                }
                prefix.push_str(target.prefix());
                let (source, key, description, nested) = match argument {
                    Argument::Digraph { first: None, .. } => {
                        ("Ctrl+k", "…", "vim_hint.digraph_first", true)
                    }
                    Argument::Digraph {
                        first: Some(first), ..
                    } => {
                        prefix.push_str("Ctrl+k ");
                        prefix.push(first);
                        return Some(SequenceHint {
                            prefix,
                            group: "vim_hint.insert_group",
                            continuations: vec![("…", "vim_hint.digraph_second", false)],
                        });
                    }
                    Argument::Literal { prefix, .. } => (prefix, "…", "vim_hint.literal_key", true),
                    Argument::Number { prefix, .. } => {
                        (prefix, "0–9 a–f", "vim_hint.literal_number", false)
                    }
                };
                prefix.push_str(source);
                ("vim_hint.insert_group", vec![(key, description, nested)])
            }
            Pending::ControlBackslash => {
                prefix.push_str("Ctrl+\\");
                let mut bindings = vec![("Ctrl+n Ctrl+g", "vim_hint.normal_mode", false)];
                if matches!(self.mode, Mode::Command | Mode::Search)
                    && self.expression_prompt.is_none()
                {
                    bindings.push(("e", "vim_hint.expression_replace", true));
                }
                ("vim_hint.mode_group", bindings)
            }
            Pending::GArgument(ref argument) => return Some(argument.hint()),
            Pending::RegisterInput { modifier } => {
                prefix.push_str("Ctrl+r");
                if let Some(modifier) = modifier {
                    prefix.push_str(" Ctrl+");
                    prefix.push(modifier);
                }
                let mut bindings = vec![("a–z 0–9 \" % # : . /", "vim_hint.register_value", false)];
                if self.expression_prompt.is_none() {
                    bindings.push(("=", "vim_hint.expression_register", true));
                }
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
            Pending::Argument('q', ..) => {
                prefix.push('q');
                (
                    "vim_hint.record_group",
                    vec![
                        (":", "vim_hint.command_window", false),
                        ("/ ?", "vim_hint.search_window", false),
                        ("a–z A–Z 0–9 \"", "vim_hint.record_register", false),
                    ],
                )
            }
            Pending::Argument(key @ ('f' | 'F' | 't' | 'T' | 'r'), operator, _) => {
                if let Some(operator) = operator {
                    push_operator(&mut prefix, operator);
                }
                prefix.push(key);
                let mut bindings = vec![
                    ("…", "vim_hint.literal_key", false),
                    ("Ctrl+k", "vim_hint.digraph_first", true),
                ];
                if key == 'r' {
                    bindings.push(("Ctrl+v Ctrl+q", "vim_hint.literal_key", true));
                    bindings.push(("Ctrl+e Ctrl+y", "vim_hint.replace_adjacent", false));
                }
                ("vim_hint.character_group", bindings)
            }
            Pending::Argument('@', ..) => {
                prefix.push('@');
                (
                    "vim_hint.macro_group",
                    vec![
                        ("a–z 0–9 \"", "vim_hint.play_register", false),
                        ("@", "vim_hint.repeat_playback", false),
                        (":", "vim_hint.repeat_ex", false),
                        ("=", "vim_hint.expression_execute", true),
                    ],
                )
            }
            Pending::Argument('"', ..) => {
                prefix.push('"');
                (
                    "vim_hint.register_group",
                    vec![
                        ("a–z 0–9 \" % # : . /", "vim_hint.register_value", false),
                        ("=", "vim_hint.expression_register", true),
                    ],
                )
            }
            Pending::Argument(key @ ('m' | '\'' | '`'), operator, _) => {
                if let Some(operator) = operator {
                    push_operator(&mut prefix, operator);
                }
                prefix.push(key);
                let bindings = if key == 'm' {
                    vec![("a–z A–Z [ ] < > ' `", "vim_hint.set_mark", false)]
                } else {
                    vec![("a–z A–Z 0–9 [ ] < > ( ) { }", "vim_g.mark_argument", false)]
                };
                ("vim_g.mark_group", bindings)
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
        assert!(
            hint.continuations
                .contains(&("n", "vim_hint.syntax_child", false))
        );
        vim.key("w", &mut register);
        assert!(vim.sequence_hint().is_none());
        vim.key("f", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "f");
        assert!(
            hint.continuations
                .contains(&("Ctrl+k", "vim_hint.digraph_first", true))
        );
    }

    /// Syntax nodes use translated text-object entries and Visual-only sibling motions.
    #[test]
    fn syntax_hints_follow_visual_and_operator_modes() {
        let mut vim = Vim::new(Document::new("alpha"));
        let mut register = Register::default();
        vim.key("[", &mut register);
        assert!(
            !vim.sequence_hint()
                .unwrap()
                .continuations
                .iter()
                .any(|item| item.0 == "n")
        );
        vim.key("Escape", &mut register);
        vim.text("v[", &mut register);
        assert!(vim.sequence_hint().unwrap().continuations.contains(&(
            "n",
            "vim_hint.syntax_previous",
            false
        )));
        vim.key("Escape", &mut register);
        vim.text("da", &mut register);
        let hint = vim.sequence_hint().unwrap();
        assert_eq!(hint.prefix, "da");
        assert!(
            hint.continuations
                .contains(&("n", "vim_hint.syntax_parent", false))
        );
    }
}
