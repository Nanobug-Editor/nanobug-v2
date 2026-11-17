//! Expand terminal-mode definitions using the ordinary modal matching tables.

use super::{
    KEY_LIMIT, ModeGroup, PendingMappingHint, RECURSION_LIMIT, Register, Vim, mapping_key,
};

/// One literal PTY key or an Ex command produced by terminal-mode mapping expansion.
#[derive(Debug, PartialEq, Eq)]
pub enum TerminalStroke {
    Key(String),
    Command(String),
}

const GROUPS: [Option<ModeGroup>; 2] = [Some(ModeGroup::Terminal), None];

/// Bound recursive work independently of the length of each replacement.
struct Expansion {
    strokes: Vec<TerminalStroke>,
    remaining: usize,
}

impl Vim {
    /// Bypass per-grapheme matching when no terminal definitions apply in this buffer.
    pub fn has_terminal_mappings(&self, register: &Register) -> bool {
        !self.input_maps.terminal_held.is_empty()
            || !self
                .input_maps
                .local
                .read()
                .unwrap()
                .table(ModeGroup::Terminal)
                .is_empty()
            || !register
                .input_maps
                .0
                .read()
                .unwrap()
                .tables
                .table(ModeGroup::Terminal)
                .is_empty()
    }

    /// Resolve committed terminal input; return None to use the native key or text fast path.
    pub fn terminal_mapping_key(
        &mut self,
        key: String,
        register: &Register,
    ) -> Option<Vec<TerminalStroke>> {
        if self.input_maps.terminal_held.is_empty() {
            let (_, exact, waiting) =
                self.match_mapping(GROUPS, std::slice::from_ref(&key), register);
            if exact.is_none() && !waiting {
                return None;
            }
        }
        let mut keys = std::mem::take(&mut self.input_maps.terminal_held);
        keys.push(key);
        Some(self.resolve_terminal_keys(&keys, true, register))
    }

    /// Release the currently held terminal prefix using the exact match or literal keys.
    pub fn finish_terminal_mapping(&mut self, register: &Register) -> Vec<TerminalStroke> {
        let keys = std::mem::take(&mut self.input_maps.terminal_held);
        self.resolve_terminal_keys(&keys, false, register)
    }

    /// Use the common buffer identity and input serial for the terminal prefix timeout.
    pub fn terminal_mapping_token(&self) -> Option<(u64, u64)> {
        (!self.input_maps.terminal_held.is_empty())
            .then_some((self.doc.opening, self.input_maps.serial))
    }

    /// Describe reachable terminal definitions for the shared delayed sequence popup.
    pub fn terminal_mapping_hint(&self, register: &Register) -> Option<PendingMappingHint> {
        self.build_mapping_hint(&self.input_maps.terminal_held, GROUPS, GROUPS, register)
    }

    /// Expand this batch, permitting only original keys to hold a new timeout.
    fn resolve_terminal_keys(
        &mut self,
        keys: &[String],
        wait: bool,
        register: &Register,
    ) -> Vec<TerminalStroke> {
        let mut expansion = Expansion {
            strokes: Vec::new(),
            remaining: KEY_LIMIT,
        };
        self.expand_terminal_keys(keys, true, wait, 0, register, &mut expansion);
        expansion.strokes
    }

    /// Follow recursive rules in order while preserving non-remappable right-hand input.
    fn expand_terminal_keys(
        &mut self,
        mut keys: &[String],
        remap: bool,
        wait: bool,
        depth: usize,
        register: &Register,
        expansion: &mut Expansion,
    ) {
        while let Some((key, tail)) = keys.split_first() {
            if expansion.remaining == 0 || depth >= RECURSION_LIMIT {
                self.message = crate::i18n::text("vim_mapping.recursive").into();
                return;
            }
            expansion.remaining -= 1;
            if key == "<Cmd>" {
                let Some(end) = tail.iter().position(|key| key == "Enter") else {
                    self.message = crate::i18n::text("vim_mapping.command_unterminated").into();
                    return;
                };
                let text = tail[..end]
                    .iter()
                    .map(|key| if key == "Tab" { "\t" } else { key })
                    .collect();
                expansion.strokes.push(TerminalStroke::Command(text));
                keys = &tail[end + 1..];
                continue;
            }
            let (used, rule, waiting) = if remap {
                self.match_mapping(GROUPS, keys, register)
            } else {
                (0, None, false)
            };
            if wait && waiting {
                self.input_maps.terminal_held = keys.to_vec();
                self.input_maps.serial = self.input_maps.serial.wrapping_add(1);
                return;
            }
            if let Some((_, rule)) = rule {
                let replacement = self.mapping_replacement(&rule, register);
                let mut replacement = replacement.as_slice();
                if rule.recursive
                    && replacement.len() >= used
                    && keys[..used]
                        .iter()
                        .zip(replacement)
                        .all(|(left, right)| mapping_key(left) == mapping_key(right))
                {
                    expansion
                        .strokes
                        .push(TerminalStroke::Key(replacement[0].clone()));
                    replacement = &replacement[1..];
                }
                self.expand_terminal_keys(
                    replacement,
                    rule.recursive,
                    false,
                    depth + 1,
                    register,
                    expansion,
                );
                keys = &keys[used..];
            } else {
                expansion.strokes.push(TerminalStroke::Key(key.clone()));
                keys = tail;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// Match delayed printable keys and physical chords, then remove both definitions.
    #[test]
    fn terminal_mappings_resolve_and_clear_in_their_own_mode() {
        let mut vim = Vim::new(Document::new(""));
        let mut register = Register::default();
        vim.execute_ex_line("tmap <F8> jj", &mut register);
        vim.execute_ex_line("tnoremap jj <Cmd>echo 'terminal'<CR>", &mut register);
        let command = TerminalStroke::Command("echo 'terminal'".into());
        assert_eq!(
            vim.terminal_mapping_key("F8".into(), &register),
            Some(vec![command])
        );
        assert_eq!(
            vim.terminal_mapping_key("j".into(), &register),
            Some(Vec::new())
        );
        assert_eq!(
            vim.terminal_mapping_hint(&register).unwrap().continuations[0].keys,
            "j"
        );
        assert_eq!(
            vim.finish_terminal_mapping(&register),
            [TerminalStroke::Key("j".into())]
        );
        vim.execute_ex_line("tunmap jj", &mut register);
        assert!(vim.terminal_mapping_key("j".into(), &register).is_none());
        vim.execute_ex_line("tmapclear", &mut register);
        assert!(vim.terminal_mapping_key("F8".into(), &register).is_none());
    }
}
