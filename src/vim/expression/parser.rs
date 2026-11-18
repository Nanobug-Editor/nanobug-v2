//! Parse Vim expression operators and call read-only builtins.

use super::{VALUE_LIMIT, Value};
use crate::vim::{Register, Vim};
use std::{cell::Cell, collections::BTreeMap};

/// Tokenize and evaluate source against an editor, registers, and named local bindings.
pub(super) fn evaluate(
    vim: &Vim,
    source: &str,
    register: &Register,
    locals: &[(&str, Value)],
    command_pos: Option<&Cell<Option<usize>>>,
) -> Result<Value, String> {
    let mut parser = prepare(vim, source, register, locals, command_pos)?;
    let value = parser.parse(0, true, 0)?;
    if parser.tokens.peek().is_some() {
        return Err("Unexpected text after expression".into());
    }
    Ok(value)
}

/// Evaluate adjacent Vim expressions from the same command line.
pub(super) fn evaluate_list(
    vim: &Vim,
    source: &str,
    register: &Register,
    locals: &[(&str, Value)],
) -> Result<Vec<Value>, String> {
    let mut parser = prepare(vim, source, register, locals, None)?;
    let mut values = Vec::new();
    while parser.tokens.peek().is_some() {
        values.push(parser.parse(0, true, 0)?);
    }
    Ok(values)
}

/// Tokenize `source` and bind it to the editor state needed for evaluation.
fn prepare<'editor, 'source>(
    vim: &'editor Vim,
    mut source: &'source str,
    register: &'editor Register,
    locals: &'editor [(&'editor str, Value)],
    command_pos: Option<&'editor Cell<Option<usize>>>,
) -> Result<Parser<'editor, 'source>, String> {
    if source.len() > 64 * 1024 {
        return Err("Expression is too long".into());
    }
    let mut tokens = Vec::new();
    while !source.trim_start().is_empty() {
        let (token, rest) = token(source.trim_start())?;
        tokens.push(token);
        source = rest;
    }
    Ok(Parser {
        tokens: tokens.into_iter().peekable(),
        vim,
        register,
        locals,
        command_pos,
    })
}

/// A literal, identifier, register reference, or operator.
enum Token<'a> {
    /// Fully decoded text or number.
    Value(Value),
    /// Name of a builtin, local argument, or constant.
    Name(&'a str),
    /// A read-only reference to a Vim register.
    Register(char),
    /// An Ex option reference with an optional g: or l: scope.
    Option(&'a str),
    /// Syntactic symbol, including an operator or delimiter.
    Symbol(&'static str),
}

/// Vim syntax tokens, longest forms first where they share a prefix.
const SYMBOLS: &[&str] = &[
    "==#", "==?", "!=#", "!=?", "=~#", "=~?", "!~#", "!~?", "==", "!=", "<=", ">=", "=~", "!~",
    "&&", "||", "..", "#{", "+", "-", "*", "/", "%", "!", "<", ">", "(", ")", "[", "]", "{", "}",
    ",", "?", ":", ".",
];

/// Read one token at the start of nonblank source and return the remaining input.
fn token(source: &str) -> Result<(Token<'_>, &str), String> {
    let ch = source.chars().next().expect("nonempty source");
    if matches!(ch, '"' | '\'') {
        let (value, rest) = quoted_string(source, ch)?;
        return Ok((Token::Value(Value::String(value)), rest));
    }
    if ch.is_ascii_digit() {
        let (number, size) = numeric_token(source)?;
        return Ok((Token::Value(number), &source[size..]));
    }
    if let Some(rest) = source.strip_prefix('@')
        && let Some(name) = rest.chars().next()
    {
        return Ok((Token::Register(name), &rest[name.len_utf8()..]));
    }
    if let Some(rest) = source.strip_prefix('&')
        && !rest.starts_with('&')
    {
        let name = rest
            .strip_prefix("g:")
            .or_else(|| rest.strip_prefix("l:"))
            .unwrap_or(rest);
        let length = name.bytes().take_while(u8::is_ascii_alphanumeric).count();
        if length > 0 {
            return Ok((Token::Option(&name[..length]), &name[length..]));
        }
    }
    if ch.is_ascii_alphabetic() || ch == '_' {
        let name_len = source
            .bytes()
            .take_while(|&b| b.is_ascii_alphanumeric() || b == b'_')
            .count();
        let mut len = name_len;
        if name_len == 1
            && source.as_bytes().get(1) == Some(&b':')
            && matches!(ch, 'a' | 'b' | 'g' | 's' | 'l' | 't' | 'v' | 'w')
        {
            let name_end = source[2..]
                .bytes()
                .take_while(|&b| b.is_ascii_alphanumeric() || b == b'_')
                .count();
            if name_end != 0 {
                len = 2 + name_end;
            }
        }
        let name = &source[..len];
        if matches!(name, "is" | "isnot") {
            return Ok((
                Token::Symbol(if name == "is" { "is" } else { "isnot" }),
                &source[len..],
            ));
        }
        return Ok((Token::Name(name), &source[len..]));
    }
    for &symbol in SYMBOLS {
        if let Some(rest) = source.strip_prefix(symbol) {
            return Ok((Token::Symbol(symbol), rest));
        }
    }
    Err(format!("Unexpected character {ch:?}"))
}

/// Decode a single- or double-quoted Vim String and return the remaining source.
fn quoted_string(source: &str, quote: char) -> Result<(String, &str), String> {
    let mut value = String::new();
    let mut rest = &source[1..];
    while let Some(ch) = rest.chars().next() {
        rest = &rest[ch.len_utf8()..];
        if ch == quote {
            if quote == '\'' && rest.starts_with('\'') {
                value.push('\'');
                rest = &rest[1..];
                continue;
            }
            return Ok((value, rest));
        }
        if ch != '\\' || quote == '\'' {
            value.push(ch);
            continue;
        }
        let escape = rest.chars().next().ok_or("Incomplete String escape")?;
        rest = &rest[escape.len_utf8()..];
        match escape {
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            'b' => value.push('\u{8}'),
            'e' => value.push('\u{1b}'),
            '<' => {
                let Some((name, tail)) = rest.split_once('>') else {
                    value.push('<');
                    continue;
                };
                if let Some(special) = special_key(name) {
                    value.push(special);
                    rest = tail;
                } else {
                    value.push('<');
                }
            }
            'x' | 'X' | 'u' | 'U' | '0'..='7' => {
                let (radix, digits, max) = match escape {
                    'x' | 'X' => (16, rest, 2),
                    'u' => (16, rest, 4),
                    'U' => (16, rest, 8),
                    _ => (8, &source[source.len() - rest.len() - 1..], 3),
                };
                let count = digits
                    .chars()
                    .take(max)
                    .take_while(|ch| ch.is_digit(radix))
                    .count();
                if count == 0 {
                    value.push(escape);
                    continue;
                }
                let n = u32::from_str_radix(&digits[..count], radix).map_err(|e| e.to_string())?;
                value.push(char::from_u32(n).ok_or("Invalid character code")?);
                rest = &digits[count..];
            }
            _ => value.push(escape),
        }
    }
    Err("Unterminated String".into())
}

/// Translate the control characters and named keys representable in Vim text registers.
fn special_key(name: &str) -> Option<char> {
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "esc" => Some('\u{1b}'),
        "cr" | "enter" => Some('\r'),
        "nl" => Some('\n'),
        "tab" => Some('\t'),
        "bs" => Some('\u{8}'),
        "space" => Some(' '),
        "lt" => Some('<'),
        "up" => Some('\u{f700}'),
        "down" => Some('\u{f701}'),
        "left" => Some('\u{f702}'),
        "right" => Some('\u{f703}'),
        _ => {
            let [ascii] = name.strip_prefix("c-")?.as_bytes() else {
                return None;
            };
            ascii
                .is_ascii_lowercase()
                .then(|| char::from(*ascii - b'a' + 1))
        }
    }
}

/// Return the parsed Number or Float and the length of its leading literal.
fn numeric_token(text: &str) -> Result<(Value, usize), String> {
    if let Some((radix, prefix)) = radix_prefix(text) {
        let digits = text[prefix..]
            .bytes()
            .take_while(|&b| char::from(b).is_digit(radix))
            .count();
        let parsed = i64::from_str_radix(&text[prefix..prefix + digits], radix)
            .map_err(|e| e.to_string())?;
        return Ok((Value::Integer(parsed), prefix + digits));
    }
    let whole = text.bytes().take_while(u8::is_ascii_digit).count();
    if text.as_bytes().get(whole) == Some(&b'.')
        && text
            .as_bytes()
            .get(whole + 1)
            .is_some_and(u8::is_ascii_digit)
    {
        let mut end = whole
            + 1
            + text[whole + 1..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
        if matches!(text.as_bytes().get(end), Some(b'e' | b'E')) {
            end += 1;
            if matches!(text.as_bytes().get(end), Some(b'+' | b'-')) {
                end += 1;
            }
            end += text[end..].bytes().take_while(u8::is_ascii_digit).count();
        }
        let value = text[..end].parse::<f64>().map_err(|e| e.to_string())?;
        return Ok((Value::Float(value), end));
    }
    let digits = &text[..whole];
    let radix = if digits.starts_with('0') && digits.bytes().all(|b| matches!(b, b'0'..=b'7')) {
        8
    } else {
        10
    };
    let parsed = i64::from_str_radix(digits, radix).map_err(|e| e.to_string())?;
    Ok((Value::Integer(parsed), whole))
}

/// Recognize explicit radix prefixes.
fn radix_prefix(text: &str) -> Option<(u32, usize)> {
    if text.len() < 2 || text.as_bytes()[0] != b'0' {
        return None;
    }
    let radix = match text.as_bytes()[1] {
        b'x' | b'X' => 16,
        b'o' | b'O' => 8,
        b'b' | b'B' => 2,
        _ => return None,
    };
    Some((radix, 2))
}

/// Convert from the leading digits in a string as Vim's arithmetic does.
pub(super) fn string_number(text: &str) -> i64 {
    let text = text.trim_start();
    let (text, negative) = match text.as_bytes().first() {
        Some(b'-') => (&text[1..], true),
        Some(b'+') => (&text[1..], false),
        _ => (text, false),
    };
    let (radix, prefix) = radix_prefix(text).unwrap_or((10, 0));
    let digits = text[prefix..]
        .bytes()
        .take_while(|&b| char::from(b).is_digit(radix))
        .count();
    let value = i64::from_str_radix(&text[prefix..prefix + digits], radix).unwrap_or(0);
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// Precedence parser with access to read-only editor state and supplied locals.
struct Parser<'editor, 'source> {
    /// Pending tokens, with one symbol of lookahead.
    tokens: std::iter::Peekable<std::vec::IntoIter<Token<'source>>>,
    /// Buffer and cursor from which pure helpers read.
    vim: &'editor Vim,
    /// Registers from which expressions may read.
    register: &'editor Register,
    /// Scoped values such as a:type supplied by a caller.
    locals: &'editor [(&'editor str, Value)],
    /// Caret setter available only during Ctrl+\ e command-line replacement.
    command_pos: Option<&'editor Cell<Option<usize>>>,
}

impl<'source> Parser<'_, 'source> {
    /// Consume the next symbol when it matches expected.
    fn take(&mut self, expected: &str) -> bool {
        if matches!(self.tokens.peek(), Some(Token::Symbol(symbol)) if *symbol == expected) {
            self.tokens.next();
            true
        } else {
            false
        }
    }

    /// Require symbol before continuing.
    fn expect(&mut self, symbol: &str) -> Result<(), String> {
        self.take(symbol)
            .then_some(())
            .ok_or_else(|| format!("Expected {symbol:?}"))
    }

    /// Parse from the minimum precedence, optionally bypassing execution of an unused branch.
    fn parse(&mut self, precedence: u8, active: bool, depth: usize) -> Result<Value, String> {
        if depth > 80 {
            return Err("Expression is too deeply nested".into());
        }
        let mut value = match self.tokens.next().ok_or("Expected an expression")? {
            Token::Value(value) => value,
            Token::Register(name) if active => {
                Value::String(self.vim.text_register(Some(name), self.register).text)
            }
            Token::Register(_) => Value::Null,
            Token::Option(name) if active => match name {
                "verbose" | "vbs" => Value::Integer(self.register.input_maps.ex_verbose().into()),
                "verbosefile" | "vfile" => Value::String(
                    self.register
                        .input_maps
                        .with_options(|options| options.verbosefile.clone()),
                ),
                _ => return Err(format!("Unknown option: {name}")),
            },
            Token::Option(_) => Value::Null,
            Token::Name(name) if self.take("(") => {
                let args = self.sequence(")", active, depth)?;
                if active {
                    self.call(name, args)?
                } else {
                    Value::Null
                }
            }
            Token::Name(name) if active => self.name(name)?,
            Token::Name(_) => Value::Null,
            Token::Symbol("(") => {
                let value = self.parse(0, active, depth + 1)?;
                self.expect(")")?;
                value
            }
            Token::Symbol("[") => Value::List(self.sequence("]", active, depth)?),
            Token::Symbol(symbol @ ("{" | "#{")) => {
                self.dictionary(symbol == "#{", active, depth)?
            }
            Token::Symbol(op @ ("!" | "-" | "+")) => {
                let value = self.parse(8, active, depth + 1)?;
                if active {
                    unary(op, value)?
                } else {
                    Value::Null
                }
            }
            Token::Symbol(symbol) => return Err(format!("Unexpected {symbol:?}")),
        };
        while let Some(Token::Symbol(symbol)) = self.tokens.peek() {
            check_size(value.size_hint())?;
            let symbol = *symbol;
            if symbol == "[" {
                self.tokens.next();
                let key = self.parse(0, active, depth + 1)?;
                if self.take(":") {
                    let end = if self.take("]") {
                        None
                    } else {
                        let end = self.parse(0, active, depth + 1)?;
                        self.expect("]")?;
                        Some(end)
                    };
                    if active {
                        value = slice(value, key, end)?;
                    }
                } else {
                    self.expect("]")?;
                    if active {
                        value = index(value, key)?;
                    }
                }
                continue;
            }
            let priority = match symbol {
                "?" => 1,
                "||" => 2,
                "&&" => 3,
                "==" | "==#" | "==?" | "!=" | "!=#" | "!=?" | "<" | ">" | "<=" | ">=" | "=~"
                | "=~#" | "=~?" | "!~" | "!~#" | "!~?" | "is" | "isnot" => 4,
                "+" | "-" | ".." | "." => 5,
                "*" | "/" | "%" => 6,
                _ => break,
            };
            if priority < precedence {
                break;
            }
            self.tokens.next();
            if symbol == "?" {
                let condition = active && value.number()? != 0;
                let yes = self.parse(0, condition, depth + 1)?;
                self.expect(":")?;
                let no = self.parse(1, active && !condition, depth + 1)?;
                value = if condition { yes } else { no };
            } else {
                let skip = active
                    && match symbol {
                        "&&" => value.number()? == 0,
                        "||" => value.number()? != 0,
                        _ => false,
                    };
                let rhs = self.parse(priority + 1, active && !skip, depth + 1)?;
                if active {
                    value = if skip {
                        Value::Integer(i64::from(symbol == "||"))
                    } else {
                        binary(symbol, value, rhs)?
                    };
                }
            }
        }
        check_size(value.size_hint())?;
        Ok(value)
    }

    /// Parse a comma-separated sequence through its closer.
    fn sequence(&mut self, close: &str, active: bool, depth: usize) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        let mut size = 0usize;
        while !self.take(close) {
            let value = self.parse(0, active, depth + 1)?;
            size = size.saturating_add(value.size_hint()).saturating_add(2);
            check_size(size)?;
            values.push(value);
            if self.take(close) {
                break;
            }
            self.expect(",")?;
        }
        Ok(values)
    }

    /// Parse a keyed value expression, allowing bare keys in legacy literal dictionaries.
    fn dictionary(&mut self, literal: bool, active: bool, depth: usize) -> Result<Value, String> {
        let mut values = BTreeMap::new();
        let mut size = 0usize;
        while !self.take("}") {
            let key = if literal {
                match self.tokens.next() {
                    Some(Token::Name(name)) => name.to_owned(),
                    _ => return Err("Expected a literal Dictionary key".into()),
                }
            } else {
                self.parse(0, active, depth + 1)?.display()
            };
            self.expect(":")?;
            let value = self.parse(0, active, depth + 1)?;
            size = size
                .saturating_add(key.len().saturating_mul(2))
                .saturating_add(6)
                .saturating_add(value.size_hint());
            check_size(size)?;
            if values.insert(key, value).is_some() && active {
                return Err("Duplicate Dictionary key".into());
            }
            if self.take("}") {
                break;
            }
            self.expect(",")?;
        }
        Ok(Value::Dictionary(values))
    }

    /// Find name in scoped bindings or Vim constants.
    fn name(&self, name: &str) -> Result<Value, String> {
        Ok(match name {
            "v:true" => Value::Bool(true),
            "v:false" => Value::Bool(false),
            "v:null" | "v:none" => Value::Null,
            "v:errmsg" => Value::String(self.register.input_maps.last_ex_error()),
            _ => self
                .locals
                .iter()
                .rev()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
                .or_else(|| {
                    self.register
                        .script_variable(self.vim.doc.buffer_number(), name)
                })
                .ok_or_else(|| format!("Undefined variable: {name}"))?,
        })
    }

    /// Invoke one pure builtin with its evaluated arguments.
    fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        use Value::{Integer, List, String as S};
        let text = |v: &Value| v.display();
        let count = |v: &Value| v.number().map(|n| n.max(0) as usize);
        let cursor = self.vim.doc.cursor;
        let (row, _) = self.vim.doc.location(cursor);
        let line = |arg: &Value| match arg {
            S(arg) if arg == "." => Ok((row + 1) as i64),
            S(arg) if arg == "$" => Ok(self.vim.line_count() as i64),
            S(arg) if arg.starts_with('\'') => Ok(arg[1..]
                .chars()
                .next()
                .and_then(|name| self.vim.mark_motion(name, false))
                .map_or(0, |motion| self.vim.doc.location(motion.end).0 as i64 + 1)),
            _ => arg.number(),
        };
        let column = |arg: &Value| match arg {
            S(arg) if arg == "." => Ok((cursor - self.vim.doc.starts[row] + 1) as i64),
            S(arg) if arg == "$" => Ok((self.vim.doc.line(row).len() + 1) as i64),
            S(arg) if arg.starts_with('\'') => Ok(arg[1..]
                .chars()
                .next()
                .and_then(|name| self.vim.mark_motion(name, false))
                .map_or(0, |motion| {
                    let row = self.vim.doc.location(motion.end).0;
                    (motion.end - self.vim.doc.starts[row] + 1) as i64
                })),
            _ => Err("Invalid column position".to_owned()),
        };
        let at_line = |at: i64| {
            usize::try_from(at)
                .ok()
                .filter(|&at| at > 0 && at <= self.vim.line_count())
        };
        Ok(match (name, args.as_slice()) {
            ("len", [List(values)]) => Integer(values.len() as i64),
            ("len", [Value::Dictionary(values)]) => Integer(values.len() as i64),
            ("len" | "strlen", [value]) => Integer(text(value).len() as i64),
            ("strchars", [value]) => Integer(text(value).chars().count() as i64),
            ("string", [value]) => S(value.quoted()),
            ("str2nr", [value]) => Integer(string_number(&text(value))),
            ("str2nr", [value, base]) => Integer(
                i64::from_str_radix(
                    &text(value),
                    base.number()?
                        .try_into()
                        .ok()
                        .filter(|n| (2..=36).contains(n))
                        .ok_or("Invalid Number radix")?,
                )
                .unwrap_or(0),
            ),
            ("toupper", [value]) => S(text(value).to_uppercase()),
            ("tolower", [value]) => S(text(value).to_lowercase()),
            ("trim", [value]) => S(text(value).trim().to_owned()),
            ("abs", [Value::Float(value)]) => Value::Float(value.abs()),
            ("abs", [value]) => Integer(value.number()?.wrapping_abs()),
            ("empty", [value]) => Integer(i64::from(is_empty(value))),
            ("line", [arg]) => Integer(line(arg)?),
            ("col", [arg]) => Integer(column(arg)?),
            ("getpos", [arg]) => List(vec![
                Integer(0),
                Integer(line(arg)?),
                Integer(column(arg)?),
                Integer(0),
            ]),
            ("getline", [arg]) => S(at_line(line(arg)?)
                .map_or("", |at| self.vim.doc.line(at - 1))
                .to_owned()),
            ("getline", [first, last]) => {
                let start = line(first)?.max(1);
                let end = line(last)?.min(self.vim.line_count() as i64);
                let mut lines = Vec::new();
                let mut size = 0usize;
                for at in (start..=end).filter_map(at_line) {
                    let line = self.vim.doc.line(at - 1);
                    size = size
                        .saturating_add(line.len().saturating_mul(2))
                        .saturating_add(4);
                    check_size(size)?;
                    lines.push(S(line.to_owned()));
                }
                List(lines)
            }
            ("getcmdline", []) => S(self.vim.command.clone()),
            ("getcmdtype", []) => S(
                if self.vim.mode.accepts_text() && !self.vim.mode.is_insert() {
                    self.vim.prompt().to_string()
                } else {
                    String::new()
                },
            ),
            ("getcmdpos", []) => Integer(
                if matches!(
                    self.vim.mode,
                    crate::vim::Mode::Command | crate::vim::Mode::Search
                ) {
                    (self.vim.command_cursor() + 1) as i64
                } else {
                    0
                },
            ),
            ("setcmdpos", [pos]) => {
                let pos = pos.number()?;
                if let Some(caret) = self.command_pos
                    && pos > 0
                {
                    caret.set(Some((pos - 1) as usize));
                    Integer(0)
                } else {
                    Integer(1)
                }
            }
            ("mode", []) => S(match self.vim.mode {
                crate::vim::Mode::Normal => "n",
                crate::vim::Mode::Command | crate::vim::Mode::Search => "c",
                crate::vim::Mode::Visual => "v",
                crate::vim::Mode::VisualLine => "V",
                crate::vim::Mode::VisualBlock => "\u{16}",
                crate::vim::Mode::Insert => "i",
                _ => "R",
            }
            .into()),
            ("getreg", []) => S(self.register.read(None).text),
            ("getreg", [key]) => S(self
                .vim
                .text_register(text(key).chars().next(), self.register)
                .text),
            ("expand", [key]) if matches!(text(key).as_str(), "%" | "#") => S(self
                .vim
                .text_register(text(key).chars().next(), self.register)
                .text),
            ("expand", [key]) if matches!(text(key).as_str(), "<cword>" | "<cWORD>") => {
                S(self.word_at_cursor(text(key) == "<cWORD>").to_owned())
            }
            ("join", [List(items)]) => S(join(items, " ")?),
            ("join", [List(items), sep]) => S(join(items, &text(sep))?),
            ("split", [value]) => List(
                text(value)
                    .split_whitespace()
                    .map(|piece| S(piece.into()))
                    .collect(),
            ),
            ("split", [value, pattern]) => {
                let pattern = text(pattern);
                let regex = crate::vim::pattern::compile(&pattern, None)?;
                List(
                    regex
                        .split(&text(value))
                        .filter(|part| !part.is_empty())
                        .map(|part| S(part.into()))
                        .collect(),
                )
            }
            ("repeat", [value, amount]) => {
                let amount = count(amount)?;
                match value {
                    List(values) => {
                        check_size(value.size_hint().saturating_mul(amount))?;
                        List((0..amount).flat_map(|_| values.iter().cloned()).collect())
                    }
                    _ => {
                        let value = text(value);
                        if value.len().saturating_mul(amount) > VALUE_LIMIT {
                            return Err("Expression result is too large".into());
                        }
                        S(value.repeat(amount))
                    }
                }
            }
            ("range", [end]) => List(number_range(0, end.number()?.saturating_sub(1), 1)?),
            ("range", [start, end]) => List(number_range(start.number()?, end.number()?, 1)?),
            ("range", [start, end, step]) => List(number_range(
                start.number()?,
                end.number()?,
                step.number()?,
            )?),
            ("get", [list, key]) => index(list.clone(), key.clone()).unwrap_or(Integer(0)),
            ("get", [list, key, default]) => {
                index(list.clone(), key.clone()).unwrap_or_else(|_| default.clone())
            }
            ("printf", [format, values @ ..]) => S(printf(&text(format), values)?),
            _ => return Err(format!("Unknown function or invalid arguments: {name}()")),
        })
    }

    /// Return the ordinary or blank-delimited word containing the current editor caret.
    fn word_at_cursor(&self, big: bool) -> &str {
        let doc = &self.vim.doc;
        let cursor = doc.cursor;
        let class = self.vim.class(cursor, big);
        if class == 0 {
            return "";
        }
        let line = doc.line_range(doc.location(cursor).0);
        let mut start = cursor;
        let mut end = doc.next(cursor);
        while start > line.start && self.vim.class(doc.previous(start), big) == class {
            start = doc.previous(start);
        }
        while end < line.end && self.vim.class(end, big) == class {
            end = doc.next(end);
        }
        &doc.text[start..end]
    }
}

/// Reject output that would expand beyond the expression value budget.
fn check_size(bytes: usize) -> Result<(), String> {
    (bytes <= VALUE_LIMIT)
        .then_some(())
        .ok_or_else(|| "Expression result is too large".into())
}

/// Return whether this value is empty for Vim's empty() builtin.
fn is_empty(value: &Value) -> bool {
    match value {
        Value::String(value) => value.is_empty(),
        Value::List(values) => values.is_empty(),
        Value::Dictionary(values) => values.is_empty(),
        Value::Float(value) => *value == 0.,
        other => other.number() == Ok(0),
    }
}

/// Join items with sep, checking output growth before allocating.
fn join(items: &[Value], sep: &str) -> Result<String, String> {
    let mut result = String::new();
    for (index, value) in items.iter().enumerate() {
        if index != 0 {
            result.push_str(sep);
        }
        result.push_str(&value.display());
        if result.len() > VALUE_LIMIT {
            return Err("Expression result is too large".into());
        }
    }
    Ok(result)
}

/// Build an inclusive sequence between start and end with a signed step.
fn number_range(mut start: i64, end: i64, step: i64) -> Result<Vec<Value>, String> {
    if step == 0 {
        return Err("Range step must be nonzero".into());
    }
    let mut values = Vec::new();
    while if step > 0 { start <= end } else { start >= end } {
        if values.len() == 65_536 {
            return Err("Expression result is too large".into());
        }
        values.push(Value::Integer(start));
        let Some(next) = start.checked_add(step) else {
            break;
        };
        start = next;
    }
    Ok(values)
}

/// Format basic Vim printf placeholders in format with a value per conversion.
fn printf(format: &str, values: &[Value]) -> Result<String, String> {
    let mut input = format.chars();
    let mut vals = values.iter();
    let mut output = String::new();
    while let Some(ch) = input.next() {
        if ch != '%' {
            output.push(ch);
            continue;
        }
        let kind = input.next().ok_or("Incomplete printf conversion")?;
        if kind == '%' {
            output.push('%');
            continue;
        }
        let value = vals.next().ok_or("Not enough printf arguments")?;
        let text = match kind {
            's' => value.display(),
            'd' | 'i' => value.number()?.to_string(),
            'x' => format!("{:x}", value.number()?),
            'X' => format!("{:X}", value.number()?),
            'o' => format!("{:o}", value.number()?),
            'f' => format!("{:.6}", float(value)?),
            'c' => char::from_u32(value.number()? as u32)
                .unwrap_or('\u{fffd}')
                .to_string(),
            _ => return Err(format!("Unsupported printf conversion: %{kind}")),
        };
        output.push_str(&text);
        if output.len() > VALUE_LIMIT {
            return Err("Expression result is too large".into());
        }
    }
    Ok(output)
}

/// Evaluate a prefix operator on a scalar value.
fn unary(op: &str, value: Value) -> Result<Value, String> {
    match (op, value) {
        ("!", value) => Ok(Value::Integer(i64::from(value.number()? == 0))),
        ("-", Value::Float(value)) => Ok(Value::Float(-value)),
        ("+", Value::Float(value)) => Ok(Value::Float(value)),
        ("-", value) => Ok(Value::Integer(value.number()?.wrapping_neg())),
        (_, value) => Ok(Value::Integer(value.number()?)),
    }
}

/// Compare the left and right values or evaluate their binary arithmetic operation.
fn binary(op: &str, lhs: Value, rhs: Value) -> Result<Value, String> {
    use Value::{Integer, List, String as S};
    let logical = |result| Ok(Integer(i64::from(result)));
    match op {
        "&&" | "||" => logical(rhs.number()? != 0),
        "." | ".." => {
            let left = lhs.display();
            let right = rhs.display();
            if left.len().saturating_add(right.len()) > VALUE_LIMIT {
                return Err("Expression result is too large".into());
            }
            Ok(S(left + &right))
        }
        "+" if matches!((&lhs, &rhs), (List(_), List(_))) => {
            let (List(mut left), List(right)) = (lhs, rhs) else {
                unreachable!()
            };
            if left.len().saturating_add(right.len()) > 65_536 {
                return Err("Expression result is too large".into());
            }
            left.extend(right);
            Ok(List(left))
        }
        "==" | "==#" | "==?" | "!=" | "!=#" | "!=?" | "is" | "isnot" => {
            let equal = match (&lhs, &rhs) {
                (S(left), S(right)) if op.ends_with('?') => {
                    left.to_lowercase() == right.to_lowercase()
                }
                (Integer(_), _) | (_, Integer(_)) if !op.starts_with("is") => {
                    if matches!(lhs, Value::Float(_)) || matches!(rhs, Value::Float(_)) {
                        float(&lhs)? == float(&rhs)?
                    } else {
                        lhs.number()? == rhs.number()?
                    }
                }
                _ => lhs == rhs,
            };
            logical(equal != (op.starts_with("!=") || op == "isnot"))
        }
        "<" | ">" | "<=" | ">=" => {
            let order = match (&lhs, &rhs) {
                (S(left), S(right)) => Some(left.cmp(right)),
                (Value::Float(_), _) | (_, Value::Float(_)) => {
                    float(&lhs)?.partial_cmp(&float(&rhs)?)
                }
                _ => Some(lhs.number()?.cmp(&rhs.number()?)),
            };
            logical(order.is_some_and(|order| match op {
                "<" => order.is_lt(),
                ">" => order.is_gt(),
                "<=" => order.is_le(),
                _ => order.is_ge(),
            }))
        }
        "=~" | "=~#" | "=~?" | "!~" | "!~#" | "!~?" => {
            let pattern = rhs.display();
            let regex = crate::vim::pattern::compile(&pattern, Some(op.ends_with('?')))?;
            logical(regex.is_match(&lhs.display()) != op.starts_with('!'))
        }
        "+" | "-" | "*" | "/" | "%" => {
            if matches!(lhs, Value::Float(_)) || matches!(rhs, Value::Float(_)) {
                let (l, r) = (float(&lhs)?, float(&rhs)?);
                return Ok(Value::Float(match op {
                    "+" => l + r,
                    "-" => l - r,
                    "*" => l * r,
                    "/" => l / r,
                    "%" => l % r,
                    _ => unreachable!(),
                }));
            }
            let (l, r) = (lhs.number()?, rhs.number()?);
            Ok(Integer(match op {
                "+" => l.wrapping_add(r),
                "-" => l.wrapping_sub(r),
                "*" => l.wrapping_mul(r),
                "/" if r == 0 => {
                    if l < 0 {
                        i64::MIN
                    } else if l > 0 {
                        i64::MAX
                    } else {
                        i64::MIN
                    }
                }
                "/" => l.wrapping_div(r),
                "%" if r == 0 => 0,
                "%" => l.wrapping_rem(r),
                _ => unreachable!(),
            }))
        }
        _ => unreachable!(),
    }
}

/// Coerce a Number or Float for arithmetic with a floating point operand.
fn float(value: &Value) -> Result<f64, String> {
    if let Value::Float(n) = value {
        Ok(*n)
    } else {
        Ok(value.number()? as f64)
    }
}

/// Resolve a zero-based or negative index within length.
fn relative_index(index: i64, length: usize) -> i64 {
    if index < 0 {
        length as i64 + index
    } else {
        index
    }
}

/// Return the item of value at the numeric index or dictionary key.
fn index(value: Value, key: Value) -> Result<Value, String> {
    match value {
        Value::String(text) => {
            let at = key.number()?;
            Ok(Value::String(
                usize::try_from(at)
                    .ok()
                    .and_then(|at| text.get(at..))
                    .and_then(|tail| tail.chars().next())
                    .map_or_else(String::new, |ch| ch.to_string()),
            ))
        }
        Value::List(items) => {
            let at = relative_index(key.number()?, items.len());
            usize::try_from(at)
                .ok()
                .and_then(|at| items.get(at))
                .cloned()
                .ok_or("List index out of range".into())
        }
        Value::Dictionary(mut entries) => entries
            .remove(&key.display())
            .ok_or("Dictionary key not found".into()),
        _ => Err("Expected a String, List, or Dictionary".into()),
    }
}

/// Return an inclusive String or List slice with Vim's negative index rules.
fn slice(value: Value, first: Value, last: Option<Value>) -> Result<Value, String> {
    let range = |length: usize| -> Result<std::ops::Range<usize>, String> {
        let first = relative_index(first.number()?, length).clamp(0, length as i64) as usize;
        let last = last
            .as_ref()
            .map(Value::number)
            .transpose()?
            .map_or(length as i64, |n| {
                relative_index(n, length).saturating_add(1)
            })
            .clamp(first as i64, length as i64) as usize;
        Ok(first..last)
    };
    match value {
        Value::String(value) => {
            let range = range(value.len())?;
            Ok(Value::String(
                value[value.floor_char_boundary(range.start)..value.floor_char_boundary(range.end)]
                    .into(),
            ))
        }
        Value::List(value) => Ok(Value::List(value[range(value.len())?].to_vec())),
        _ => Err("Expected a String or List to slice".into()),
    }
}
