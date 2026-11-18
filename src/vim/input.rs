//! Parse Normal and Visual commands and route native text to the active editing mode.

use super::{
    Action, Mode, Operator, Pending, Register, ScreenMotionResolver, Vim, folds::FOLD_OPERATOR,
    replay::Stroke,
};

/// Operator key for a block yank that omits trailing whitespace.
pub(super) const TRIM_YANK_OPERATOR: char = '\u{1}';
use unicode_segmentation::UnicodeSegmentation;

impl Vim {
    /// Feed a sequence of typed graphemes, for callers driving modal commands as text.
    pub fn text(&mut self, text: &str, register: &mut Register) -> Action {
        for key in text.graphemes(true) {
            let action = self.key(key, register);
            if action != Action::None {
                return action;
            }
        }
        Action::None
    }

    /// Send a single grapheme or a symbolic `key` such as Escape, Enter or Ctrl+r.
    pub fn key(&mut self, key: &str, register: &mut Register) -> Action {
        self.key_with_screen_rows(key, register, &mut |_, _, _, _| None)
    }

    /// Dispatch a native `key` using `register` and `resolve_screen` for measured vertical motion.
    /// Replay uses the resolver for each motion at its current position.
    pub fn key_with_screen_rows(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        self.set_input_options(&register.input_maps);
        if self.substitute_confirmation_active() {
            return self.dispatch(Stroke::Key(key.to_owned()), register, resolve_screen);
        }
        self.dispatch_typed(key, register, resolve_screen)
    }

    /// Commit native `text` to the active field or pass one grapheme to a pending command.
    /// Leave the command pending for another attempt if a commit contains multiple graphemes.
    pub fn commit_text(&mut self, text: &str, register: &mut Register) -> Action {
        if self.accepts_text() && !text.is_empty() && self.map_committed_text(text, register) {
            if self.awaits_literal_argument()
                && (text.graphemes(true).count() != 1 || text.contains(['\r', '\n', '\0']))
            {
                return Action::None;
            }
            let mut action = Action::None;
            for key in text.graphemes(true) {
                action = self.dispatch_typed(key, register, &mut |_, _, _, _| None);
                if action != Action::None {
                    break;
                }
            }
            return action;
        }
        self.paste_text(text, register)
    }

    /// Commit `text` literally using `register`, bypassing typed language maps and abbreviations.
    pub fn paste_text(&mut self, text: &str, register: &mut Register) -> Action {
        if !self.accepts_text() || text.is_empty() {
            return Action::None;
        }
        let action = self.flush_input_prefix(register, &mut |_, _, _, _| None);
        if action != Action::None || !self.accepts_text() {
            return action;
        }
        if self.awaits_literal_argument()
            && (text.graphemes(true).count() != 1 || text.contains(['\r', '\n', '\0']))
        {
            return Action::None;
        }
        self.dispatch(
            Stroke::Text(text.to_owned()),
            register,
            &mut |_, _, _, _| None,
        )
    }

    /// Apply recorded or newly committed `text` using `register` for modal operations.
    pub(super) fn handle_text(&mut self, text: &str, register: &mut Register) -> Action {
        if self.collecting_ex_lines() {
            let mut lines = text.split_inclusive('\n');
            while let Some(line) = lines.next() {
                let without_lf = line
                    .strip_suffix('\n')
                    .map(|line| line.trim_end_matches('\r'));
                self.command_insert(without_lf.unwrap_or(line));
                if without_lf.is_some() {
                    let submitted = std::mem::take(&mut self.command);
                    self.accept_ex_line(&submitted);
                    if !self.collecting_ex_lines() {
                        break;
                    }
                }
            }
            return Action::None;
        }
        if self.awaits_literal_argument() {
            return self.handle_key(text, register, &mut |_, _, _, _| None);
        } else if self.g.select {
            self.replace_select(text, register);
        } else if self.mode.is_insert() {
            self.insert_text(text);
        } else if self.mode.accepts_text() {
            self.command_insert(text.lines().next().unwrap_or(""));
        }
        Action::None
    }

    /// Apply `key` to the current mode and consume prefixes with `register` and `resolve_screen`.
    pub(super) fn handle_key(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let was_pending = self.select_resume;
        let action = self.handle_mode_key(key, register, resolve_screen);
        if was_pending && !self.awaiting_key() {
            self.select_resume = false;
            self.g.select = self.mode.is_visual();
        }
        action
    }

    /// Dispatch `key` within the current mode using `register` and the display row resolver.
    fn handle_mode_key(
        &mut self,
        key: &str,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if self.collecting_ex_lines() {
            match key {
                "Enter" | "Ctrl+j" | "Ctrl+m" => {
                    let line = std::mem::take(&mut self.command);
                    self.accept_ex_line(&line);
                }
                "Escape" | "Ctrl+[" | "Ctrl+c" => self.finish_ex_input(),
                "Ctrl+f" | "Up" | "Down" | "Ctrl+n" | "Ctrl+p" => {}
                _ => return self.command_key(key, register, resolve_screen),
            }
            return Action::None;
        }
        if self.substitute_confirmation_active() {
            return self.substitute_confirmation_key(key, register, resolve_screen);
        }
        if matches!(key, "Ctrl+6" | "Ctrl+^") && self.mode.accepts_text() && self.pending.is_none()
        {
            self.toggle_language();
            return Action::None;
        }
        if let Some(action) = self.maybe_expand_abbreviation(key, register, resolve_screen) {
            return action;
        }
        if let Some(Pending::InsertArgument(argument)) = self.pending {
            self.pending = None;
            return self.insert_argument(argument, key, register);
        }
        let key = if matches!(self.pending, Some(Pending::ControlBackslash)) {
            self.pending = None;
            match key {
                "e" if matches!(self.mode, Mode::Command | Mode::Search) => {
                    self.begin_expression(super::expression::Target::ReplaceLine);
                    return Action::None;
                }
                "Ctrl+n" => "Escape",
                "Ctrl+g" => "Escape",
                _ => key,
            }
        } else if key == "Ctrl+\\" && self.pending.is_none() {
            self.pending = Some(Pending::ControlBackslash);
            return Action::None;
        } else {
            key
        };
        if matches!(key, "Escape" | "Ctrl+[" | "Ctrl+c") {
            if self.mode == Mode::Command {
                Self::cancel_function_definition(register);
            }
            if self.cancel_expression() {
                return Action::None;
            }
            self.g.select = false;
            if self.mode.is_insert() {
                self.pending = None;
                let events = self.finish_insert();
                if !events.is_empty() {
                    register.last_insert = events;
                }
            } else {
                self.end_prompt_search();
                self.mode = Mode::Normal;
                self.cancel_pending();
                self.command.clear();
                self.command_pos = 0;
                self.command_selection = None;
                self.filter_range = None;
                self.clamp_normal();
            }
            return Action::None;
        }
        self.clear_ex_message();
        if self.expression_prompt.is_some() && matches!(key, "Enter" | "Ctrl+j" | "Ctrl+m") {
            return self.submit_expression(register, resolve_screen);
        }
        if self.g.select {
            match key {
                "Ctrl+o" => {
                    self.g.select = false;
                    self.select_resume = true;
                }
                "Ctrl+g" => self.g.select = false,
                "Delete" | "Backspace" => {
                    self.g.select = false;
                    self.visual_operator('d', register);
                }
                "Left" | "Right" | "Up" | "Down" | "Home" | "End" => {
                    if let Some(motion) = self.cursor_motion(key, None) {
                        self.apply_motion(motion);
                    }
                }
                _ if key.graphemes(true).count() == 1 => self.replace_select(key, register),
                _ => {}
            }
            return Action::None;
        }
        if matches!(self.mode, Mode::Command | Mode::Search) {
            return self.command_key(key, register, resolve_screen);
        }
        if self.mode.is_insert() {
            if matches!(self.pending, Some(Pending::InsertCompletion)) {
                self.pending = None;
                match key {
                    "Ctrl+o" | "Ctrl+u" => return Action::PluginComplete,
                    "Ctrl+d" | "Ctrl+f" | "Ctrl+i" | "Tab" | "Ctrl+k" | "Ctrl+l" | "Ctrl+n"
                    | "Ctrl+p" | "Ctrl+q" | "Ctrl+r" | "Ctrl+s" | "s" | "Ctrl+t" | "Ctrl+v" => {
                        let key = if key == "Tab" { "Ctrl+i" } else { key };
                        return self.insert_candidates(key, register);
                    }
                    "Ctrl+e" | "Ctrl+y" => {
                        self.scroll(key, None);
                        return Action::None;
                    }
                    "Ctrl+z" => return Action::None,
                    _ => {}
                }
            }
            if matches!(self.pending, Some(Pending::InsertControlG)) {
                self.pending = None;
                self.insert_control_g(key);
                return Action::None;
            }
            if key == "Ctrl+g" {
                self.pending = Some(Pending::InsertControlG);
                return Action::None;
            }
            if matches!(self.pending, Some(Pending::RegisterInput { .. })) {
                self.register_input_key(key, register);
                return Action::None;
            }
            if key == "Ctrl+r" {
                self.pending = Some(Pending::RegisterInput { modifier: None });
                return Action::None;
            }
            if key == "Ctrl+x" {
                self.pending = Some(Pending::InsertCompletion);
                return Action::None;
            }
            if key == "Ctrl+o" {
                self.insert_resume = Some(self.suspend_insert());
                return Action::None;
            }
            if matches!(key, "Ctrl+a" | "Ctrl+@" | "Ctrl+space") {
                self.replay_insert(&register.last_insert);
                if key != "Ctrl+a" && !register.last_insert.is_empty() {
                    self.finish_insert();
                }
                return Action::None;
            }
            if matches!(key, "Ctrl+n" | "Ctrl+p") {
                return self.insert_candidates(key, register);
            }
            if matches!(
                key,
                "Ctrl+k" | "Ctrl+v" | "Ctrl+q" | "Ctrl+Shift+v" | "Ctrl+Shift+q"
            ) {
                self.begin_literal_input(key);
                return Action::None;
            }
            if matches!(key, "PageUp" | "PageDown") && self.scroll(key, None) {
                return Action::None;
            }
            self.insert_key(key);
            return Action::None;
        }
        if key == "Delete"
            && matches!(self.pending, None | Some(Pending::Operator(_)))
            && let Some(count) = self.count
        {
            self.count = (count >= 10).then_some(count / 10);
            return Action::None;
        }
        if matches!(self.pending, None | Some(Pending::Operator(_)))
            && let [digit @ b'0'..=b'9'] = key.as_bytes()
            && (*digit != b'0' || self.count.is_some())
        {
            self.count = Some((self.count.unwrap_or(0) * 10 + usize::from(digit - b'0')).min(9999));
            return Action::None;
        }
        let count = self.count.take();
        let pending = self.pending.take();
        let selecting_register = matches!(pending, Some(Pending::Argument('"', _, _)));
        let action = match pending {
            Some(Pending::Operator(operator)) => {
                self.operator_key(operator, key, count, register);
                Action::None
            }
            Some(Pending::Prefix(prefix, operator, count)) => {
                self.prefix_key(prefix, key, operator, count, register, resolve_screen)
            }
            Some(Pending::GArgument(argument)) => self.g_argument(argument, key, register),
            Some(
                Pending::InsertControlG | Pending::InsertCompletion | Pending::InsertArgument(_),
            ) => Action::None,
            Some(Pending::ControlBackslash) => Action::None,
            Some(Pending::RegisterInput { .. }) => Action::None,
            Some(Pending::ZHeight { rows, count }) => {
                match key.as_bytes() {
                    [digit @ b'0'..=b'9'] => {
                        let rows = rows * 10 + usize::from(*digit - b'0');
                        self.pending = Some(Pending::ZHeight {
                            rows: rows.min(9999),
                            count,
                        });
                    }
                    _ if matches!(key, "Enter" | "Ctrl+m") => self.resize_view_height(rows),
                    _ if key == "Delete" => {
                        self.pending = Some(Pending::ZHeight {
                            rows: rows / 10,
                            count,
                        })
                    }
                    _ if matches!(key, "h" | "l" | "Left" | "Right") => {
                        let steps = combine_count(count, (rows > 0).then_some(rows));
                        self.scroll(&format!("z{key}"), Some(steps));
                    }
                    _ => {}
                }
                Action::None
            }
            Some(Pending::SpellUndo(count)) => {
                self.spelling_undo_key(key, count);
                Action::None
            }
            Some(Pending::SpellSuggestions(suggestions)) => {
                self.spelling_choice(key, suggestions);
                Action::None
            }
            Some(Pending::Argument(prefix, operator, count)) => {
                self.argument_key(prefix, key, operator, count, register, resolve_screen)
            }
            Some(Pending::Object {
                around,
                operator,
                count,
            }) => {
                if key == "n" {
                    use super::syntax_selection::Direction;
                    let direction = if around {
                        Direction::Parent
                    } else {
                        Direction::Child
                    };
                    self.select_syntax(direction, count, operator, register)
                } else if let Some((range, kind)) = self.object(key, around, count) {
                    if let Some(operator) = operator {
                        self.operate_object(operator, range, kind, register);
                    } else {
                        self.mode = if kind == super::motions::Kind::Line {
                            Mode::VisualLine
                        } else {
                            Mode::Visual
                        };
                        self.anchor = range.start;
                        self.set_cursor(self.doc.previous(range.end).max(range.start));
                    }
                    Action::None
                } else {
                    Action::None
                }
            }
            None => self.normal_key(key, count, register, resolve_screen),
        };
        if !selecting_register && self.pending.is_none() && !self.mode.accepts_text() {
            self.selected_register = None;
        }
        action
    }

    /// Insert the register or object requested by `key` into the current text input.
    pub(super) fn register_input_key(&mut self, key: &str, register: &Register) {
        let Some(Pending::RegisterInput { modifier }) = self.pending.take() else {
            return;
        };
        let in_prompt = !self.mode.is_insert();
        if modifier.is_none()
            && matches!(key, "Ctrl+r" | "Ctrl+o" | "Ctrl+p")
            && !(in_prompt && key == "Ctrl+p")
        {
            self.pending = Some(Pending::RegisterInput {
                modifier: key.chars().last(),
            });
            return;
        }
        if key == "=" {
            self.begin_expression(super::expression::Target::Insert(modifier));
            return;
        }
        let (text, linewise) = if in_prompt
            && matches!(key, "Ctrl+w" | "Ctrl+a" | "Ctrl+l" | "Ctrl+f" | "Ctrl+p")
        {
            let row = self.doc.location(self.doc.cursor).0;
            let text = match key {
                "Ctrl+l" => self.doc.line(row).to_owned(),
                "Ctrl+f" | "Ctrl+p" => self
                    .path_under_cursor(false)
                    .map(|(path, _)| path)
                    .unwrap_or_default(),
                _ => self
                    .object(if key == "Ctrl+w" { "w" } else { "W" }, false, 1)
                    .map(|(range, _)| self.doc.text[range].to_owned())
                    .unwrap_or_default(),
            };
            (text, false)
        } else if let [name] = key.as_bytes()
            && matches!(*name, b'"' | b'-' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'%' | b'#' | b':' | b'.' | b'/' | b'+' | b'*' | b'_')
        {
            let name = char::from(*name);
            if name == '.' && !in_prompt && modifier.is_none() {
                self.replay_insert(&register.last_insert);
                return;
            }
            let value = self.text_register(Some(name), register);
            (value.text, value.kind == super::register::Kind::Line)
        } else {
            return;
        };
        self.insert_register_value(&text, linewise, modifier);
    }

    /// Insert register text as normal typed input or as a literal according to modifier.
    pub(super) fn insert_register_value(
        &mut self,
        text: &str,
        linewise: bool,
        modifier: Option<char>,
    ) {
        if !self.mode.is_insert() {
            if modifier.is_some() {
                self.command_insert(text.lines().next().unwrap_or(""));
            } else {
                for grapheme in text.lines().next().unwrap_or("").graphemes(true) {
                    if grapheme == "\u{8}" {
                        let pos = self.command_cursor();
                        let previous = self.command[..pos]
                            .grapheme_indices(true)
                            .next_back()
                            .map_or(pos, |(index, _)| index);
                        self.command.replace_range(previous..pos, "");
                        self.command_pos = previous;
                    } else {
                        self.command_insert(grapheme);
                    }
                }
            }
        } else if matches!(modifier, Some('o' | 'p')) {
            self.insert_literal_register(text, linewise, modifier == Some('p'));
        } else if modifier.is_some() {
            self.insert_text(text);
        } else {
            for grapheme in text.graphemes(true) {
                match grapheme {
                    "\u{8}" => self.insert_key("Backspace"),
                    "\n" | "\r" | "\r\n" => self.insert_key("Enter"),
                    "\u{f700}" => self.insert_key("Up"),
                    "\u{f701}" => self.insert_key("Down"),
                    "\u{f702}" => self.insert_key("Left"),
                    "\u{f703}" => self.insert_key("Right"),
                    _ => self.insert_text(grapheme),
                }
            }
        }
    }

    /// Read a `name` from `register` or from the current file, alternate file or search state.
    pub(super) fn text_register(
        &self,
        name: Option<char>,
        register: &Register,
    ) -> super::register::Contents {
        let text = match name {
            Some('%') => self.doc.path.as_deref().map(|path| path.to_string_lossy()),
            Some('#') => self
                .command_prompt
                .alternate_path
                .as_deref()
                .map(|path| path.to_string_lossy()),
            Some('/') => self.search_pattern().map(std::borrow::Cow::Borrowed),
            _ => return register.read(name),
        };
        super::register::Contents {
            text: text.map_or_else(String::new, std::borrow::Cow::into_owned),
            kind: super::register::Kind::Character,
        }
    }

    /// Complete `operator` with `key` and the optional motion count.
    fn operator_key(
        &mut self,
        operator: Operator,
        key: &str,
        count: Option<usize>,
        register: &mut Register,
    ) {
        let combined = operator
            .count
            .or(count)
            .map(|_| combine_count(operator.count, count));
        match key {
            "v" | "V" | "Ctrl+v" => {
                let force = match key {
                    "v" => Mode::Visual,
                    "V" => Mode::VisualLine,
                    _ => Mode::VisualBlock,
                };
                self.pending = Some(Pending::Operator(Operator {
                    count: combined,
                    force: (operator.force != Some(force)).then_some(force),
                    ..operator
                }));
            }
            key if key.len() == 1
                && (key.starts_with(operator.key)
                    || operator.key == TRIM_YANK_OPERATOR && key == "y"
                    || super::g_commands::g_operator_name(operator.key)
                        .is_some_and(|name| name.strip_prefix('g') == Some(key))) =>
            {
                self.repeat_operator(operator, combined.unwrap_or(1), register);
            }
            "g" | "z" | "[" | "]" => {
                self.pending = Some(Pending::Prefix(first(key), Some(operator), combined))
            }
            "f" | "F" | "t" | "T" | "'" | "`" => {
                self.pending = Some(Pending::Argument(first(key), Some(operator), combined));
            }
            "i" | "a" => {
                self.pending = Some(Pending::Object {
                    around: key == "a",
                    operator: Some(operator),
                    count: combined.unwrap_or(1),
                });
            }
            "/" | "?" => {
                self.open_prompt(first(key), combined.unwrap_or(1));
                self.search_operator = Some(operator);
            }
            "n" | "N" => {
                if let Some(motion) = self.search_motion(key == "N", combined.unwrap_or(1)) {
                    self.operate_motion(operator, motion, register);
                }
            }
            _ => {
                if let Some(motion) = self.operator_motion(operator, key, count) {
                    self.operate_motion(operator, motion, register);
                }
            }
        }
    }

    /// Complete `prefix` and `key`, optionally inside `operator` and with `count`, using `register`
    /// and the measured vertical motion resolver `resolve_screen`.
    fn prefix_key(
        &mut self,
        prefix: char,
        key: &str,
        operator: Option<Operator>,
        count: Option<usize>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let command = match (prefix, key) {
            ('z', "Ctrl+j" | "Ctrl+m") => "zEnter".to_owned(),
            _ => format!("{prefix}{key}"),
        };
        if prefix == 'z' && operator.is_none() {
            if let [digit @ b'0'..=b'9'] = key.as_bytes() {
                self.pending = Some(Pending::ZHeight {
                    rows: usize::from(*digit - b'0'),
                    count,
                });
                return Action::None;
            }
            if self.scroll(&command, count) || self.fold_key(&command, count) {
                return Action::None;
            }
        }
        match command.as_str() {
            "[n" | "]n" if self.mode.is_visual() && operator.is_none() => {
                use super::syntax_selection::Direction;
                let direction = if prefix == '[' {
                    Direction::Previous
                } else {
                    Direction::Next
                };
                return self.select_syntax(direction, count.unwrap_or(1), None, register);
            }
            "gd" if operator.is_none() && self.mode == Mode::Normal => {
                return Action::PluginDefinition;
            }
            "gO" if operator.is_none() && self.mode == Mode::Normal => {
                return Action::PluginOutline;
            }
            "ZZ" => {
                return if self.doc.modified() {
                    Action::SaveClose
                } else {
                    Action::Close(false)
                };
            }
            "ZQ" => return Action::Close(true),
            "zu" if operator.is_none() => {
                self.pending = Some(Pending::SpellUndo(count));
                return Action::None;
            }
            "zg" | "zG" | "zw" | "zW" | "z=" if operator.is_none() => {
                self.spelling_command(key, count);
                return Action::None;
            }
            "zy" if operator.is_none() => {
                if self.mode.is_visual() {
                    self.visual_operator(TRIM_YANK_OPERATOR, register);
                } else {
                    self.pending =
                        Some(Pending::Operator(Operator::new(TRIM_YANK_OPERATOR, count)));
                }
                return Action::None;
            }
            "zp" | "zP" if operator.is_none() => {
                if self.read_only {
                    return self.reject_edit();
                }
                self.put(register, key == "P", count.unwrap_or(1), true);
                return Action::None;
            }
            _ => {}
        }
        if prefix == 'g'
            && let Some(action) = self.g_key(key, operator, count, register, resolve_screen)
        {
            return action;
        }
        if matches!(prefix, '[' | ']')
            && operator.is_none()
            && let Some(action) = self.bracket_action(&command, count, register)
        {
            return action;
        }
        if let Some(operator) = operator
            && operator.key == FOLD_OPERATOR
            && matches!(command.as_str(), "zf" | "zF")
        {
            self.line_operator(operator.key, count.unwrap_or(1), register);
            return Action::None;
        }
        if let Some(motion) = self
            .fold_motion(&command, count)
            .or_else(|| self.bracket_motion(&command, count, operator.is_some()))
            .or_else(|| self.motion(&command, count))
        {
            if let Some(operator) = operator {
                self.operate_motion(operator, motion, register);
            } else {
                self.apply_motion(motion);
            }
        }
        Action::None
    }

    /// Apply `key` to a find, mark, register, macro or replace `prefix` with optional `operator` and
    /// `count`. Macro playback uses `register` and `resolve_screen`.
    fn argument_key(
        &mut self,
        prefix: char,
        key: &str,
        operator: Option<Operator>,
        count: Option<usize>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        let n = count.unwrap_or(1);
        if key == "Ctrl+k" && matches!(prefix, 'f' | 'F' | 't' | 'T' | 'r')
            || matches!(key, "Ctrl+v" | "Ctrl+q") && prefix == 'r'
        {
            use super::insert_argument::Target;
            let target = if prefix == 'r' {
                Target::Replace(n, false)
            } else {
                Target::Find(prefix, n, operator)
            };
            self.begin_character_argument(key, target);
            return Action::None;
        }
        let name = match key.as_bytes() {
            &[byte] => Some(char::from(byte)),
            _ => None,
        };
        let motion = match prefix {
            'f' | 'F' | 't' | 'T' => self.find_motion(prefix, key, n),
            '\'' | '`' => {
                return name.map_or(Action::None, |name| {
                    self.command_mark(name, prefix == '\'', true, operator, register)
                });
            }
            'm' => {
                if let Some(name) = name.filter(|name| {
                    name.is_ascii_alphabetic() || matches!(name, '[' | ']' | '<' | '>' | '\'' | '`')
                }) {
                    self.set_command_mark(name, register);
                }
                None
            }
            'r' => {
                if matches!(key, "Ctrl+e" | "Ctrl+y") {
                    self.replace_from_adjacent_row(key == "Ctrl+e", n);
                } else {
                    self.replace_chars(
                        match key {
                            "Enter" | "Ctrl+j" | "Ctrl+m" => "\n",
                            "Tab" | "Ctrl+i" => "\t",
                            _ => key,
                        },
                        n,
                    );
                }
                None
            }
            '"' => {
                if name == Some('=') {
                    self.begin_expression(super::expression::Target::Select(count));
                    return Action::None;
                }
                self.selected_register = name.filter(|ch| {
                    ch.is_ascii_alphanumeric()
                        || matches!(
                            ch,
                            '"' | '_' | '-' | '+' | '*' | ':' | '.' | '%' | '#' | '/'
                        )
                });
                self.count = count;
                None
            }
            'q' => {
                if let Some(prompt @ (':' | '/' | '?')) = name {
                    return Action::CommandWindow(prompt);
                }
                if let Some(name) = name.filter(|name| name.is_ascii_alphanumeric() || *name == '"')
                {
                    register.recording = Some((name, Vec::new()));
                }
                None
            }
            '@' => {
                if name == Some('=') {
                    self.begin_expression(super::expression::Target::Execute(n));
                    return Action::None;
                }
                let name = if name == Some('@') {
                    register.last_played
                } else {
                    name
                };
                if let Some(name) = name {
                    return self.play_macro(name, n, register, resolve_screen);
                }
                None
            }
            _ => None,
        };
        if let Some(motion) = motion {
            if let Some(operator) = operator {
                self.operate_motion(operator, motion, register);
            } else {
                self.apply_motion(motion);
            }
        }
        Action::None
    }

    /// Dispatch a Normal or Visual `key` with `count`, or arm its next argument.
    /// Repeat and macro replay use `register` and `resolve_screen`.
    fn normal_key(
        &mut self,
        key: &str,
        count: Option<usize>,
        register: &mut Register,
        resolve_screen: &mut ScreenMotionResolver<'_>,
    ) -> Action {
        if self.mode == Mode::Normal && !self.read_only {
            match key {
                "Ctrl+]" => return Action::PluginDefinition,
                "K" => return Action::PluginHover,
                _ => {}
            }
        }
        let n = count.unwrap_or(1);
        let visual = self.mode.is_visual();
        if !visual {
            match key {
                "Ctrl+6" | "Ctrl+^" => return Action::AlternateBuffer(count.map(|n| n as u64)),
                "Ctrl+tab" | "Ctrl+Tab" => return Action::TabNavigate(0, None),
                _ => {}
            }
        }
        if self.mode == Mode::Normal
            && key == "Enter"
            && let Some(action) = self.ex_options_enter()
        {
            return action;
        }
        if self.mode == Mode::Normal
            && key == "Enter"
            && self.open_fold_at_header(self.doc.location(self.doc.cursor).0)
        {
            return Action::None;
        }
        if self.move_on_fold_preview(key, n) {
            return Action::None;
        }
        if self.read_only
            && (matches!(
                key,
                "I" | "A"
                    | "R"
                    | "r"
                    | "d"
                    | "c"
                    | "<"
                    | ">"
                    | "="
                    | "!"
                    | "."
                    | "x"
                    | "X"
                    | "Delete"
                    | "s"
                    | "S"
                    | "D"
                    | "C"
                    | "p"
                    | "P"
                    | "u"
                    | "U"
                    | "Ctrl+r"
                    | "J"
                    | "~"
                    | "&"
                    | "Ctrl+a"
                    | "Ctrl+x"
            ) || !visual && matches!(key, "i" | "Insert" | "a" | "o" | "O"))
        {
            return self.reject_edit();
        }
        if matches!(
            key,
            "i" | "Insert"
                | "a"
                | "I"
                | "A"
                | "o"
                | "O"
                | "R"
                | "r"
                | "d"
                | "c"
                | "x"
                | "X"
                | "Delete"
                | "s"
                | "S"
                | "D"
                | "C"
                | "p"
                | "P"
                | "J"
                | "~"
                | ">"
                | "<"
                | "="
        ) {
            self.edit_fold_closing_brace();
        }
        if visual {
            match key {
                "Ctrl+g" if !self.read_only => {
                    self.g.select = true;
                    return Action::None;
                }
                "x" | "Delete" | "s" | "d" | "c" | "y" | "<" | ">" | "=" | "!" => {
                    let op = match key {
                        "x" | "Delete" => 'd',
                        "s" => 'c',
                        _ => first(key),
                    };
                    self.visual_operator(op, register);
                    return Action::None;
                }
                "i" | "a" => {
                    self.pending = Some(Pending::Object {
                        around: key == "a",
                        operator: None,
                        count: n,
                    });
                    return Action::None;
                }
                "I" | "A" if self.mode == Mode::VisualBlock => {
                    let (rows, columns) = self.block_bounds();
                    self.begin_insert(Mode::Insert, false, 1, false);
                    self.set_block_insert(
                        *rows.start(),
                        *rows.end(),
                        if key == "I" {
                            columns.start
                        } else {
                            columns.end
                        },
                    );
                    return Action::None;
                }
                "o" | "O" => {
                    if self.mode == Mode::VisualBlock && key == "O" {
                        let (anchor_row, anchor_column) = self.doc.location(self.anchor);
                        let (row, column) = self.doc.location(self.doc.cursor);
                        self.anchor = self.doc.offset(anchor_row, column);
                        self.set_cursor(self.doc.offset(row, anchor_column));
                        self.desired_column = Some(anchor_column);
                    } else {
                        std::mem::swap(&mut self.anchor, &mut self.doc.cursor);
                        self.desired_column = None;
                        self.clamp_normal();
                    }
                    return Action::None;
                }
                "u" | "U" | "~" => {
                    self.case_selection(first(key));
                    return Action::None;
                }
                "Ctrl+a" | "Ctrl+x" => {
                    self.adjust_visual_numbers(key == "Ctrl+a", n, false);
                    return Action::None;
                }
                _ => {}
            }
        }
        match key {
            "i" | "Insert" if !visual => self.begin_insert(Mode::Insert, false, n, false),
            "a" | "I" | "A" | "R" => {
                let row = self.doc.location(self.doc.cursor).0;
                let line = self.doc.line_range(row);
                self.doc.cursor = match key {
                    "a" => self.doc.next(self.doc.cursor).min(line.end),
                    "I" => self.first_nonblank(row),
                    "A" => line.end,
                    _ => self.doc.cursor,
                };
                self.begin_insert(
                    if key == "R" {
                        Mode::Replace
                    } else {
                        Mode::Insert
                    },
                    false,
                    n,
                    false,
                );
            }
            "o" | "O" if !visual => self.open_line(key == "O", n),
            "v" | "V" | "Ctrl+v" => {
                let mode = match key {
                    "v" => Mode::Visual,
                    "V" => Mode::VisualLine,
                    _ => Mode::VisualBlock,
                };
                if !visual {
                    self.anchor = self.doc.cursor;
                }
                self.mode = if self.mode == mode {
                    Mode::Normal
                } else {
                    mode
                };
                self.clamp_normal();
            }
            "d" | "c" | "y" | "<" | ">" | "=" | "!" => {
                self.pending = Some(Pending::Operator(Operator::new(first(key), count)))
            }
            "g" | "z" | "Z" | "[" | "]" => {
                self.pending = Some(Pending::Prefix(first(key), None, count))
            }
            "f" | "F" | "t" | "T" | "r" | "m" | "'" | "`" | "\"" | "@" => {
                self.pending = Some(Pending::Argument(first(key), None, count));
            }
            "q" if !visual && !self.replay.playing_register() => {
                if register.recording.is_some() {
                    register.stop_recording();
                } else {
                    self.pending = Some(Pending::Argument('q', None, None));
                }
            }
            "Q" if !visual || self.mode == Mode::VisualLine => {
                if let Some(name) = register.last_recorded {
                    return self.play_macro(name, n, register, resolve_screen);
                }
            }
            "." => return self.repeat_change(count, register, resolve_screen),
            "x" | "X" | "Delete" | "s" => self.delete_chars(first(key), n, register),
            "S" => self.line_operator('c', n, register),
            "D" | "C" | "Y" => {
                let op = match key {
                    "D" => 'd',
                    "C" => 'c',
                    _ => 'y',
                };
                if let Some(motion) = self.motion("$", count) {
                    self.operate_motion(Operator::new(op, count), motion, register);
                }
            }
            "p" | "P" => self.put(register, key == "P", n, false),
            "u" | "Ctrl+r" => {
                for _ in 0..n {
                    self.doc.undo(key == "Ctrl+r");
                }
                self.replay.reset_macro_undo(self.doc.revision);
                self.desired_column = None;
                self.clamp_normal();
            }
            "U" => self.undo_line(),
            "J" => self.join_lines(n),
            "~" => self.toggle_case(n),
            ":" | "/" | "?" => self.open_prompt(first(key), n),
            "n" | "N" => self.search(key == "N", n),
            "*" | "#" => self.search_word(key == "#", n),
            "&" => self.repeat_substitution(),
            "Ctrl+a" | "Ctrl+x" => self.adjust_number(key == "Ctrl+a", n),
            "Ctrl+o" | "Ctrl+i" | "Tab" => return self.jump_history(key != "Ctrl+o", n),
            "%" if count.is_none() && self.move_between_fold_braces() => {}
            _ => {
                if let Some(motion) = self.cursor_motion(key, count) {
                    self.apply_motion(motion);
                } else {
                    self.scroll(key, count);
                }
            }
        }
        Action::None
    }

    /// Change selected graphemes' case, then return to Normal mode.
    fn case_selection(&mut self, key: char) {
        let ranges: Vec<_> = if self.mode == Mode::VisualBlock {
            let (rows, columns) = self.block_bounds();
            rows.map(|row| self.block_range(row, &columns)).collect()
        } else {
            self.selection().into_iter().collect()
        };
        let start = ranges.first().map_or(self.doc.cursor, |range| range.start);
        let revision = self.doc.revision;
        for range in ranges.into_iter().rev() {
            let text: String = self.doc.text[range.clone()]
                .chars()
                .map(|ch| {
                    if key == 'U' || key == '~' && ch.is_lowercase() {
                        ch.to_uppercase().collect::<String>()
                    } else {
                        ch.to_lowercase().collect()
                    }
                })
                .collect();
            self.replace(range, &text, self.doc.revision == revision);
        }
        self.mode = Mode::Normal;
        self.set_cursor(start);
    }
}

/// First character of a known nonempty command name.
fn first(key: &str) -> char {
    key.chars().next().expect("nonempty command")
}

/// Combine the operator and motion counts without exceeding the key sequence work bound.
fn combine_count(a: Option<usize>, b: Option<usize>) -> usize {
    a.unwrap_or(1).saturating_mul(b.unwrap_or(1)).min(10_000)
}
