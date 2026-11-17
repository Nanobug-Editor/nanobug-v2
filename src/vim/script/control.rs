//! Parse and resume Rust Ex control flow across ordinary editor commands.

mod variables;

use super::{call_parts, split_fields, split_word, statements};
use crate::vim::{Register, Vim, expression::Value, pattern};
use regex::Regex;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use variables::Binding;
pub use variables::ScriptTabScope;
pub(super) use variables::{Variables, WindowVariables};

/// A physical source line or one statement from its Ex pipeline.
pub struct ScriptLine {
    pub number: usize,
    pub text: String,
    pub literal: bool,
}

type Failure = (usize, String);
type Block = Arc<[Instruction]>;

/// An Ex program retained when ordinary commands yield to the Workspace.
pub struct ScriptProgram {
    frames: Vec<Frame>,
    locals: BTreeMap<String, Binding>,
    source: Option<String>,
    finish_source: bool,
    function: bool,
    steps: usize,
    last_line: usize,
    returned: Option<Value>,
    deferred: Vec<(String, Vec<Value>)>,
}

#[derive(Clone)]
enum Instruction {
    Line(Arc<ScriptLine>),
    If(usize, Vec<(String, Block)>, Block),
    While(usize, String, Block),
    For(usize, String, String, Block),
    Try(usize, Protected),
}

#[derive(Clone)]
struct Protected {
    body: Block,
    catches: Arc<[(Option<Regex>, Block)]>,
    finally: Block,
}

enum Frame {
    Block(Block, usize),
    While(usize, String, Block),
    For(usize, String, VecDeque<Value>, Block),
    Try(Protected, Phase),
}

enum Phase {
    Body,
    Catch(Option<Binding>),
    Finally(Option<Flow>),
}

enum Flow {
    Error(Failure),
    Break(usize),
    Continue(usize),
    Return(Value),
    Finish,
}

impl ScriptProgram {
    /// Parse numbered source statements and retain the script identity for s: variables.
    pub fn compile(lines: Vec<ScriptLine>, source: Option<String>) -> Result<Self, Failure> {
        let finish_source = source.is_some();
        Self::parse(lines.into(), source, None, finish_source)
    }

    /// Parse a user-command expansion retaining its definition context but no source to stop.
    pub fn user_command(lines: Vec<ScriptLine>, source: Option<String>) -> Result<Self, Failure> {
        Self::parse(lines.into(), source, None, false)
    }

    /// Start a function frame with its own locals and read-only arguments.
    pub(super) fn function<'a>(
        lines: impl IntoIterator<Item = &'a str>,
        arguments: BTreeMap<String, Value>,
        source: Option<String>,
    ) -> Result<Self, Failure> {
        let input = lines
            .into_iter()
            .enumerate()
            .map(|(index, text)| ScriptLine {
                number: index + 1,
                text: text.into(),
                literal: false,
            })
            .collect();
        Self::parse(input, source, Some(arguments), false)
    }

    fn parse(
        input: VecDeque<ScriptLine>,
        source: Option<String>,
        arguments: Option<BTreeMap<String, Value>>,
        finish_source: bool,
    ) -> Result<Self, Failure> {
        let function = arguments.is_some();
        let locals = arguments
            .into_iter()
            .flatten()
            .map(|(key, value)| (key, Binding::constant(value)))
            .collect();
        let block = Parser { input, depth: 0 }.block(&[])?;
        Ok(Self {
            frames: vec![Frame::Block(block, 0)],
            locals,
            source,
            finish_source,
            function,
            steps: 0,
            last_line: 0,
            returned: None,
            deferred: Vec::new(),
        })
    }

    /// Transfer the completed function's return value.
    pub(super) fn returned(&mut self) -> Option<Value> {
        self.returned.take()
    }

    /// Transfer deferred calls in reverse registration order.
    pub(super) fn deferred(&mut self) -> impl Iterator<Item = (String, Vec<Value>)> + use<> {
        std::mem::take(&mut self.deferred).into_iter().rev()
    }

    /// Give the originating file to commands that define functions or evaluate source scope.
    pub(super) fn source(&self) -> Option<String> {
        self.source.clone()
    }

    /// Advance to the next ordinary Ex line. A previous host failure can enter a catch block.
    pub fn next(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        failure: Option<String>,
    ) -> Result<Option<Arc<ScriptLine>>, Failure> {
        if let Some(message) = failure {
            self.unwind(Flow::Error((self.last_line, message)))?;
        }
        while let Some(frame) = self.frames.pop() {
            self.steps += 1;
            if self.steps > 100_000 {
                return Err((
                    self.last_line,
                    "Vim script exceeded its execution limit".into(),
                ));
            }
            match frame {
                Frame::Block(block, offset) => {
                    let Some(instruction) = block.get(offset).cloned() else {
                        continue;
                    };
                    self.frames.push(Frame::Block(block, offset + 1));
                    let number = match &instruction {
                        Instruction::Line(line) => line.number,
                        Instruction::Try(number, _)
                        | Instruction::If(number, ..)
                        | Instruction::While(number, ..)
                        | Instruction::For(number, ..) => *number,
                    };
                    self.last_line = number;
                    match self.step(vim, register, instruction) {
                        Ok(Some(line)) => return Ok(Some(line)),
                        Ok(None) => {}
                        Err(error) => self.unwind(Flow::Error((number, error)))?,
                    }
                }
                Frame::While(number, condition, body) => {
                    self.last_line = number;
                    match self.condition(vim, register, &condition) {
                        Ok(true) => {
                            self.frames
                                .push(Frame::While(number, condition, body.clone()));
                            self.frames.push(Frame::Block(body, 0));
                        }
                        Ok(false) => {}
                        Err(error) => self.unwind(Flow::Error((number, error)))?,
                    }
                }
                Frame::For(number, variables, mut values, body) => {
                    self.last_line = number;
                    if let Some(value) = values.pop_front() {
                        if let Err(error) = self.assign_for(vim, register, &variables, value) {
                            self.unwind(Flow::Error((number, error)))?;
                        } else {
                            self.frames
                                .push(Frame::For(number, variables, values, body.clone()));
                            self.frames.push(Frame::Block(body, 0));
                        }
                    }
                }
                Frame::Try(protected, Phase::Body) => self.enter_finally(protected, None),
                Frame::Try(protected, Phase::Catch(prior)) => {
                    self.restore_exception(prior);
                    self.enter_finally(protected, None);
                }
                Frame::Try(_, Phase::Finally(flow)) => {
                    if let Some(flow) = flow {
                        self.unwind(flow)?;
                    }
                }
            }
        }
        Ok(None)
    }

    /// Interpret a numeric Vim condition using this frame's variables.
    fn condition(&self, vim: &Vim, register: &Register, text: &str) -> Result<bool, String> {
        self.evaluate(vim, text, register)?
            .number()
            .map(|number| number != 0)
    }

    /// Interpret an instruction or yield an ordinary Ex line.
    fn step(
        &mut self,
        vim: &Vim,
        register: &mut Register,
        instruction: Instruction,
    ) -> Result<Option<Arc<ScriptLine>>, String> {
        match instruction {
            Instruction::If(_, branches, otherwise) => {
                let mut selected = otherwise;
                for (condition, body) in branches {
                    if self.condition(vim, register, &condition)? {
                        selected = body;
                        break;
                    }
                }
                self.frames.push(Frame::Block(selected, 0));
            }
            Instruction::While(number, condition, body) => {
                self.frames.push(Frame::While(number, condition, body))
            }
            Instruction::For(number, names, expression, body) => {
                let items = match self.evaluate(vim, &expression, register)? {
                    Value::List(items) => items.into(),
                    Value::String(text) => text
                        .chars()
                        .map(|ch| Value::String(ch.to_string()))
                        .collect(),
                    _ => return Err("A :for expression must be a List or String".into()),
                };
                self.frames.push(Frame::For(number, names, items, body));
            }
            Instruction::Try(_, protected) => {
                self.frames.push(Frame::Try(protected.clone(), Phase::Body));
                self.frames.push(Frame::Block(protected.body, 0));
            }
            Instruction::Line(line) => {
                if line.literal {
                    return Ok(Some(line));
                }
                let (word, arguments) = split_word(&line.text);
                let flow = match word {
                    "break" | "brea" => Flow::Break(line.number),
                    "continue" | "con" => Flow::Continue(line.number),
                    "throw" | "th" => {
                        return Err(self.evaluate(vim, arguments, register)?.display());
                    }
                    "execute" | "exe" => {
                        let source = self.evaluate(vim, arguments, register)?.display();
                        let lines = vim.script_lines(&source, line.number).into();
                        let block = Parser {
                            input: lines,
                            depth: 0,
                        }
                        .block(&[])
                        .map_err(|(_, error)| error)?;
                        self.frames.push(Frame::Block(block, 0));
                        return Ok(None);
                    }
                    "return" | "retu" if self.function => Flow::Return(if arguments.is_empty() {
                        Value::Integer(0)
                    } else {
                        self.evaluate(vim, arguments, register)?
                    }),
                    "finish" | "fini" | "finis" if self.finish_source => Flow::Finish,
                    "defer" if self.function => {
                        let (name, args) =
                            call_parts(arguments).ok_or("Expected a function call for :defer")?;
                        let values = split_fields(args, ',')
                            .into_iter()
                            .filter(|value| !value.is_empty())
                            .map(|value| self.evaluate(vim, value, register))
                            .collect::<Result<Vec<_>, _>>()?;
                        self.deferred.push((name.to_owned(), values));
                        return Ok(None);
                    }
                    "defer" => return Err(":defer requires a function".into()),
                    _ if self.variable_command(vim, register, word, arguments)? => return Ok(None),
                    _ => return Ok(Some(line)),
                };
                if !arguments.is_empty()
                    && matches!(flow, Flow::Break(_) | Flow::Continue(_) | Flow::Finish)
                {
                    return Err(format!("Unexpected argument to :{word}"));
                }
                self.unwind(flow).map_err(|(_, error)| error)?;
            }
        }
        Ok(None)
    }

    /// Route a jump or failure through active :try frames before leaving their scopes.
    fn unwind(&mut self, flow: Flow) -> Result<(), Failure> {
        while let Some(frame) = self.frames.pop() {
            match frame {
                Frame::While(..) | Frame::For(..) => match flow {
                    Flow::Break(_) => return Ok(()),
                    Flow::Continue(_) => {
                        self.frames.push(frame);
                        return Ok(());
                    }
                    _ => {}
                },
                Frame::Try(protected, phase) => match phase {
                    Phase::Body => {
                        if let Flow::Error((_, message)) = &flow
                            && let Some((_, body)) =
                                protected.catches.iter().find(|(pattern, _)| {
                                    pattern
                                        .as_ref()
                                        .is_none_or(|pattern| pattern.is_match(message))
                                })
                        {
                            let body = body.clone();
                            let prior = self.locals.insert(
                                "v:exception".into(),
                                Binding::mutable(Value::String(message.clone())),
                            );
                            self.frames.push(Frame::Try(protected, Phase::Catch(prior)));
                            self.frames.push(Frame::Block(body, 0));
                            return Ok(());
                        }
                        self.enter_finally(protected, Some(flow));
                        return Ok(());
                    }
                    Phase::Catch(prior) => {
                        self.restore_exception(prior);
                        self.enter_finally(protected, Some(flow));
                        return Ok(());
                    }
                    Phase::Finally(_) => {}
                },
                Frame::Block(_, _) => {}
            }
        }
        match flow {
            Flow::Error(error) => Err(error),
            Flow::Break(line) => Err((line, ":break outside a loop".into())),
            Flow::Continue(line) => Err((line, ":continue outside a loop".into())),
            Flow::Return(value) => {
                self.returned = Some(value);
                Ok(())
            }
            Flow::Finish => Ok(()),
        }
    }

    /// Enter the finally body with the preceding result saved until it completes.
    fn enter_finally(&mut self, protected: Protected, flow: Option<Flow>) {
        let body = protected.finally.clone();
        self.frames
            .push(Frame::Try(protected, Phase::Finally(flow)));
        self.frames.push(Frame::Block(body, 0));
    }

    /// Restore the exception binding that existed before this catch.
    fn restore_exception(&mut self, prior: Option<Binding>) {
        if let Some(prior) = prior {
            self.locals.insert("v:exception".into(), prior);
        } else {
            self.locals.remove("v:exception");
        }
    }
}

/// Parse nested Ex blocks terminated by the appropriate command.
struct Parser {
    input: VecDeque<ScriptLine>,
    depth: usize,
}

impl Parser {
    /// Consume instructions until a member of stops or the end of input.
    fn block(&mut self, stops: &[&str]) -> Result<Block, Failure> {
        self.depth += 1;
        if self.depth > 128 {
            return Err((
                self.input.front().map_or(0, |line| line.number),
                "Vim script nesting is too deep".into(),
            ));
        }
        let mut block = Vec::new();
        while let Some(line) = self.input.front() {
            let (word, _) = split_word(&line.text);
            if !line.literal && stops.contains(&keyword(word)) {
                break;
            }
            let line = self.input.pop_front().expect("peeked Ex line");
            let (word, rest) = split_word(&line.text);
            let instruction = match keyword(word) {
                _ if line.literal => Instruction::Line(Arc::new(line)),
                "if" => self.branch(line.number, rest)?,
                "while" => {
                    let body = self.block(&["endwhile"])?;
                    self.close("endwhile", line.number)?;
                    Instruction::While(line.number, rest.into(), body)
                }
                "for" => {
                    let separator = rest.match_indices("in").find(|&(index, _)| {
                        rest[..index].ends_with(char::is_whitespace)
                            && rest[index + 2..].starts_with(char::is_whitespace)
                    });
                    let Some((index, _)) = separator else {
                        return Err((line.number, "Expected :for name in expression".into()));
                    };
                    let body = self.block(&["endfor"])?;
                    self.close("endfor", line.number)?;
                    Instruction::For(
                        line.number,
                        rest[..index].trim().into(),
                        rest[index + 2..].trim().into(),
                        body,
                    )
                }
                "try" => self.protected(line.number)?,
                "else" | "elseif" | "endif" | "endwhile" | "endfor" | "catch" | "finally"
                | "endtry" => {
                    return Err((line.number, format!("Unexpected :{word}")));
                }
                _ => Instruction::Line(Arc::new(line)),
            };
            block.push(instruction);
        }
        self.depth -= 1;
        Ok(block.into())
    }

    /// Parse a conditional branch, all elseif alternatives, and an optional else.
    fn branch(&mut self, number: usize, first: &str) -> Result<Instruction, Failure> {
        let stops = ["elseif", "else", "endif"];
        let mut branches = vec![(first.to_owned(), self.block(&stops)?)];
        let mut otherwise = Arc::new([]) as Block;
        while let Some(line) = self.input.pop_front() {
            let (word, rest) = split_word(&line.text);
            match keyword(word) {
                "elseif" => branches.push((rest.to_owned(), self.block(&stops)?)),
                "else" => {
                    otherwise = self.block(&["endif"])?;
                    self.close("endif", line.number)?;
                    return Ok(Instruction::If(number, branches, otherwise));
                }
                "endif" => return Ok(Instruction::If(number, branches, otherwise)),
                _ => break,
            }
        }
        Err((number, "Missing :endif".into()))
    }

    /// Parse a try body, the ordered catch patterns, and an optional finally body.
    fn protected(&mut self, number: usize) -> Result<Instruction, Failure> {
        let stops = ["catch", "finally", "endtry"];
        let body = self.block(&stops)?;
        let mut catches = Vec::new();
        let mut finally = Arc::new([]) as Block;
        while let Some(line) = self.input.pop_front() {
            let (word, rest) = split_word(&line.text);
            match keyword(word) {
                "catch" => {
                    let pattern = if rest.is_empty() {
                        None
                    } else {
                        let delimiter = rest.chars().next().expect("nonempty catch pattern");
                        let (pattern, tail) =
                            pattern::delimited(&rest[delimiter.len_utf8()..], delimiter)
                                .ok_or_else(|| {
                                    (line.number, "Missing :catch pattern delimiter".into())
                                })?;
                        if !tail.trim().is_empty() {
                            return Err((line.number, "Unexpected text after :catch".into()));
                        }
                        Some(
                            pattern::compile(&pattern, None)
                                .map_err(|error| (line.number, error))?,
                        )
                    };
                    catches.push((pattern, self.block(&stops)?));
                }
                "finally" => {
                    finally = self.block(&["endtry"])?;
                    self.close("endtry", line.number)?;
                    return Ok(Instruction::Try(
                        number,
                        Protected {
                            body,
                            catches: catches.into(),
                            finally,
                        },
                    ));
                }
                "endtry" => {
                    return Ok(Instruction::Try(
                        number,
                        Protected {
                            body,
                            catches: catches.into(),
                            finally,
                        },
                    ));
                }
                _ => break,
            }
        }
        Err((number, "Missing :endtry".into()))
    }

    /// Consume the expected closing command after a block.
    fn close(&mut self, expected: &str, number: usize) -> Result<(), Failure> {
        if self
            .input
            .pop_front()
            .is_some_and(|line| keyword(split_word(&line.text).0) == expected)
        {
            Ok(())
        } else {
            Err((number, format!("Missing :{expected}")))
        }
    }
}

/// Resolve a structural command's native abbreviation.
pub(super) fn keyword(word: &str) -> &str {
    const CONTROLS: &[(&str, usize)] = &[
        ("if", 2),
        ("elseif", 5),
        ("else", 2),
        ("endif", 2),
        ("while", 2),
        ("endwhile", 4),
        ("for", 3),
        ("endfor", 4),
        ("try", 3),
        ("catch", 3),
        ("finally", 4),
        ("endtry", 4),
    ];
    CONTROLS
        .iter()
        .find(|(name, minimum)| word.len() >= *minimum && name.starts_with(word))
        .map_or(word, |(name, _)| *name)
}

/// Split local script input, retaining one-based physical line numbers.
pub(super) fn program_lines(source: &str) -> Vec<ScriptLine> {
    source
        .lines()
        .enumerate()
        .flat_map(|(line, text)| {
            statements(text).map(move |text| ScriptLine {
                number: line + 1,
                text: text.into(),
                literal: false,
            })
        })
        .collect()
}
