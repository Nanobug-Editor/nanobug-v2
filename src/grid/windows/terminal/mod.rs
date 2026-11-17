//! An interactive PTY buffer and its independent editor views.

mod input;
mod view;

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::Dimensions,
    index::Point,
    selection::Selection,
    sync::FairMutex,
    term::{Config, Term, TermMode},
    tty::{self, Options, Shell},
};
use async_channel::{Receiver, Sender};
use nanobug::animation::scroll::GridScroll;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(in crate::grid) use input::KeyResult;

/// PTY notifications consumed by the native workspace.
#[derive(Clone)]
struct Events {
    /// Bounded to coalesce redundant screen repaint requests while the UI is busy.
    sender: Sender<Event>,
    /// An exit is observable even if a burst of repaint notifications fills the channel.
    exited: Arc<AtomicBool>,
}

impl EventListener for Events {
    /// Publish actionable VT events while the desktop's frame loop owns their effects.
    fn send_event(&self, event: Event) {
        if matches!(event, Event::Exit) {
            self.exited.store(true, Ordering::Release);
        }
        if matches!(
            event,
            Event::Wakeup
                | Event::Exit
                | Event::PtyWrite(_)
                | Event::TextAreaSizeRequest(_)
                | Event::ColorRequest(..)
        ) {
            let _ = self.sender.try_send(event);
        }
    }
}

/// The screen size reported to both the emulator and the child.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Size {
    /// Physical grid columns.
    columns: u16,
    /// Physical grid rows.
    rows: u16,
}

impl Size {
    /// Use nonzero cell dimensions within the terminal protocol's range.
    fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns: columns.clamp(2, u16::MAX.into()) as u16,
            rows: rows.clamp(1, u16::MAX.into()) as u16,
        }
    }

    /// Represent a view that may expose just one column of the underlying terminal.
    fn view(columns: usize, rows: usize) -> Self {
        Self {
            columns: columns.clamp(1, u16::MAX.into()) as u16,
            rows: rows.clamp(1, u16::MAX.into()) as u16,
        }
    }

    /// Character dimensions used by ConPTY and UNIX PTYs.
    fn window(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.columns,
            cell_width: 0,
            cell_height: 0,
        }
    }
}

impl Dimensions for Size {
    /// Newly opened screens have no scrollback.
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    /// Row count, excluding scrollback.
    fn screen_lines(&self) -> usize {
        self.rows.into()
    }

    /// Column count.
    fn columns(&self) -> usize {
        self.columns.into()
    }
}

/// Process and screen retained until the last handle to the terminal buffer drops.
struct TerminalSession {
    /// Grid mutated by the PTY reader with short locks for the UI.
    screen: Arc<FairMutex<Term<Events>>>,
    /// Input, resize and shutdown lane of the PTY.
    io: EventLoopSender,
    /// Worker-owned signal published after draining final PTY output.
    exited: Arc<AtomicBool>,
}

/// Independent presentation and input state for one view of a terminal buffer.
pub(in crate::grid) struct Terminal {
    /// Process and screen shared across views of this buffer.
    session: Arc<TerminalSession>,
    /// Character dimensions of this view.
    viewport: Size,
    /// Animated viewport measured from the start of scrollback.
    scroll: GridScroll,
    /// Top row measured from the start of scrollback, or `None` to follow the tail.
    top: Option<usize>,
    /// Left column exposed by this view when the PTY is wider than its viewport.
    left: usize,
    /// Whether this view sends printable input to the PTY.
    input: bool,
    /// Review caret measured from the start of scrollback, or `None` to follow the live caret.
    review: Option<Point>,
    /// Review selection in terminal grid coordinates, rebased when the history grows.
    selection: Option<Selection>,
    /// Grid state used when the review cursor and selection were last synchronized.
    history: usize,
    /// Column count of the grid before its last resize.
    screen_columns: usize,
    /// Whether this view was last synchronized with the alternate screen.
    alternate_screen: bool,
    /// Initial Ctrl+backslash consumes itself while waiting for the next key.
    escape: bool,
    /// Resume Terminal input once the next Normal command finishes.
    once: bool,
    /// Pending g command in Terminal-Normal mode.
    goto: bool,
    /// Accumulated Normal command count.
    count: Option<usize>,
}

impl Terminal {
    /// Start a PTY in `directory` with a default shell or explicit `command`.
    /// The `columns` and `rows` describe the initial editor viewport.
    pub(in crate::grid) fn start(
        directory: PathBuf,
        command: Option<String>,
        columns: usize,
        rows: usize,
    ) -> anyhow::Result<(Self, Receiver<Event>)> {
        let (sender, receiver) = async_channel::bounded(64);
        let exit_signal = Arc::new(AtomicBool::new(false));
        let events = Events {
            sender,
            exited: exit_signal.clone(),
        };
        let size = Size::new(columns, rows);
        let config = Options {
            working_directory: Some(directory),
            shell: command.map(command_shell),
            drain_on_exit: true,
            env: [
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ]
            .into(),
            ..Options::default()
        };
        let pty = tty::new(&config, size.window(), 0)?;
        let screen = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            events.clone(),
        )));
        let input = EventLoop::new(
            Arc::clone(&screen),
            events,
            pty,
            config.drain_on_exit,
            false,
        )?;
        let io = input.channel();
        input.spawn();
        Ok((
            Self::new_view(
                Arc::new(TerminalSession {
                    screen,
                    io,
                    exited: exit_signal,
                }),
                size,
            ),
            receiver,
        ))
    }

    /// Start a Normal-mode view of `session` with the initial `viewport` dimensions.
    fn new_view(session: Arc<TerminalSession>, viewport: Size) -> Self {
        Self {
            session,
            viewport,
            scroll: GridScroll::default(),
            top: None,
            left: 0,
            input: false,
            review: None,
            selection: None,
            history: 0,
            screen_columns: viewport.columns(),
            alternate_screen: false,
            escape: false,
            once: false,
            goto: false,
            count: None,
        }
    }

    /// Open an independent review view of this buffer without starting another process.
    pub(in crate::grid) fn shared_view(&self) -> Self {
        Self::new_view(self.session.clone(), self.viewport)
    }

    /// Report the shared `columns` and `rows` to the screen and PTY, returning whether they changed.
    pub(in crate::grid) fn resize(&self, columns: usize, rows: usize) -> bool {
        if columns == 0 || rows == 0 || self.exited() {
            return false;
        }
        let size = Size::new(columns, rows);
        let mut term = self.session.screen.lock();
        if size.columns() == term.columns() && size.screen_lines() == term.screen_lines() {
            return false;
        }
        term.resize(size);
        let _ = self.session.io.send(Msg::Resize(size.window()));
        true
    }

    /// Whether keystrokes currently belong to the child.
    pub(in crate::grid) fn accepts_text(&self) -> bool {
        self.input && !self.exited()
    }

    /// Switch this view to live input or Normal review, preserving the shared emulator's mode.
    pub(in crate::grid) fn set_input(&mut self, input: bool) {
        let desired = input && !self.exited();
        if desired != self.input {
            self.review = None;
            self.top = None;
        }
        if desired {
            self.selection = None;
        }
        self.input = desired;
    }

    /// Feed native `text` to the PTY and enclose pasted text when the child requests it.
    pub(in crate::grid) fn commit(&mut self, text: &str, pasted: bool) {
        self.escape = false;
        let bracketed = pasted
            && self
                .session
                .screen
                .lock()
                .mode()
                .contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            // ESC cannot be pasted inside a bracketed-paste packet.
            let text = text.replace('\u{1b}', "");
            self.write(format!("\x1b[200~{text}\x1b[201~").into_bytes());
        } else if pasted {
            self.write(text.replace("\r\n", "\r").replace('\n', "\r").into_bytes());
        } else {
            self.write(text.as_bytes().to_vec());
        }
    }

    /// Hand bytes to the PTY reader's write queue.
    fn write(&self, bytes: Vec<u8>) {
        if !self.exited() {
            let _ = self.session.io.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    /// Deliver an emulator request `event` once for the shared process.
    pub(in crate::grid) fn event(&self, event: Event) {
        if self.exited() {
            return;
        }
        match event {
            Event::TextAreaSizeRequest(format) => {
                let term = self.session.screen.lock();
                let size = Size::new(term.columns(), term.screen_lines());
                self.write(format(size.window()).into_bytes());
            }
            Event::ColorRequest(index, format) => {
                let color = self.session.screen.lock().colors()[index];
                if let Some(color) = color {
                    self.write(format(color).into_bytes());
                }
            }
            Event::PtyWrite(text) => self.write(text.into_bytes()),
            _ => {}
        }
    }

    /// Localized footer name for the mode that currently owns keys.
    pub(in crate::grid) fn mode_label(&self) -> &'static str {
        use nanobug::i18n::text;
        if self.exited() {
            text("terminal.exited")
        } else if self.accepts_text() {
            text("terminal.input")
        } else {
            text("terminal.normal")
        }
    }

    /// Whether the PTY has quit and leaves its output in the terminal buffer.
    pub(in crate::grid) fn exited(&self) -> bool {
        self.session.exited.load(Ordering::Acquire)
    }

    /// Check emitted PTY output without disturbing the review cursor.
    #[cfg(test)]
    pub(in crate::grid) fn contains_text(&self, text: &str) -> bool {
        use alacritty_terminal::index::{Column, Line, Point};
        let term = self.session.screen.lock();
        (term.topmost_line().0..=term.bottommost_line().0).any(|row| {
            let line: String = (0..term.columns())
                .map(|column| term.grid()[Point::new(Line(row), Column(column))].c)
                .collect();
            line.contains(text)
        })
    }
}

impl Drop for TerminalSession {
    /// Stop the reader and close the console when the shared process is released.
    fn drop(&mut self) {
        let _ = self.io.send(Msg::Shutdown);
    }
}

/// Invoke an explicit `command` with the user's platform shell rather than splitting its quotes.
fn command_shell(command: String) -> Shell {
    #[cfg(windows)]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
        Shell::new(
            shell,
            vec![
                "/D".into(),
                "/S".into(),
                "/C".into(),
                format!("\"{command}\""),
            ],
        )
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        Shell::new(shell, vec!["-c".into(), command])
    }
}
