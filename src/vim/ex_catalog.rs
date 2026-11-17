//! Commands for local help metadata and application diagnostics.

use super::{Action, Vim};

/// Names offered by Ex command-line completion.
pub(super) const NAMES: &[&str] = &["checkhealth", "cquit", "helptags", "restart", "runtime"];

impl Vim {
    /// Interpret the name and arguments in one information command `text`.
    pub(super) fn ex_catalog_command(&mut self, text: &str) -> Option<Action> {
        let text = text.trim();
        let prefix = text.bytes().take_while(u8::is_ascii_digit).count();
        let (code, command) = text.split_at(prefix);
        let command = command.trim_start();
        let (name, args) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        let quit = name.trim_end_matches('!');
        if quit.len() >= 2 && "cquit".starts_with(quit) {
            let args = args.trim();
            let code = match (code.is_empty(), args.is_empty()) {
                (true, true) => Some(1),
                (true, false) => args.parse().ok(),
                (false, true) => code.parse().ok(),
                (false, false) => None,
            };
            return Some(if let Some(code) = code {
                Action::QuitWithCode(code)
            } else {
                self.ex_failed = true;
                self.message =
                    crate::i18n::format("editor.invalid_ex_argument", &[("argument", &text)]);
                Action::None
            });
        }
        let (name, args) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        if name.len() >= 4 && "restart".starts_with(name) {
            let args = args.trim();
            let (close, startup) = args.strip_prefix('+').map_or(("qall", args), |args| {
                args.split_once(char::is_whitespace).unwrap_or((args, ""))
            });
            if matches!(close, "qa" | "qall" | "qa!" | "qall!" | "wqa" | "wqall") {
                return Some(Action::Restart {
                    close: close.into(),
                    command: startup.trim().into(),
                });
            }
            self.ex_failed = true;
            self.message =
                crate::i18n::format("editor.invalid_ex_argument", &[("argument", &close)]);
            return Some(Action::None);
        }
        let (runtime, all_matches) = name
            .strip_suffix('!')
            .map_or((name, false), |bare| (bare, true));
        if runtime.len() >= 2 && "runtime".starts_with(runtime) {
            return Some(Action::Runtime {
                patterns: args.trim().into(),
                all_matches,
            });
        }
        if name.len() >= 3 && "checkhealth".starts_with(name) {
            return Some(Action::CheckHealth(args.trim().into()));
        }
        if name.len() >= 5 && "helptags".starts_with(name) {
            let args = args.trim();
            let (list_self, directory) = args
                .strip_prefix("++t ")
                .map_or((false, args), |dir| (true, dir.trim_start()));
            return Some(Action::HelpTags {
                directory: directory.into(),
                list_self,
            });
        }
        None
    }
}
