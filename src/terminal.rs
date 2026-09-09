//! One terminal session: a PTY, the alacritty grid/parser, and its I/O thread.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, EventLoopSender, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use anyhow::{Context, Result};
use winit::event_loop::EventLoopProxy;

/// Identifies a tab. Monotonically increasing, never reused.
pub type TabId = u64;

/// Reserved `UserEvent.tab` value: a dragged-out tab's drop timer expired.
pub const SYS_DROP_TIMEOUT: TabId = u64::MAX - 1;

/// Event sent from the PTY thread to the winit event loop.
#[derive(Debug)]
pub struct UserEvent {
    pub tab: TabId,
    pub event: Event,
}

/// Bridges alacritty's `EventListener` into winit's event loop.
#[derive(Clone)]
pub struct EventProxy {
    tab: TabId,
    proxy: EventLoopProxy<UserEvent>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let _ = self.proxy.send_event(UserEvent { tab: self.tab, event });
    }
}

/// Grid size in cells plus the pixel size of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

impl From<GridSize> for WindowSize {
    fn from(g: GridSize) -> Self {
        WindowSize {
            num_lines: g.rows as u16,
            num_cols: g.cols as u16,
            cell_width: g.cell_width,
            cell_height: g.cell_height,
        }
    }
}

/// What to launch in a new terminal.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Initial tab title.
    pub title: String,
}

pub struct Terminal {
    pub id: TabId,
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    sender: EventLoopSender,
    /// Title set by the running program via OSC, if any.
    pub title: Option<String>,
    /// Title given at launch (saved-command name or shell name).
    pub base_title: String,
    /// User-set title (double-click the tab); overrides everything else.
    pub custom_title: Option<String>,
    pub exited: bool,
    pub size: GridSize,
    /// Cursor animation and effects state (owned by the UI layer).
    pub view: crate::app::TermView,
}

impl Terminal {
    pub fn spawn(
        id: TabId,
        proxy: EventLoopProxy<UserEvent>,
        launch: &Launch,
        size: GridSize,
        config: TermConfig,
    ) -> Result<Self> {
        let event_proxy = EventProxy { tab: id, proxy };

        let term = Term::new(config, &size, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "kindlyTerm".to_string());

        let options = tty::Options {
            shell: Some(tty::Shell::new(launch.program.clone(), launch.args.clone())),
            working_directory: launch.cwd.clone().map(Into::into),
            drain_on_exit: false,
            env,
        };

        let pty = tty::new(&options, size.into(), id).context("spawning pty")?;
        let pty_loop = PtyEventLoop::new(Arc::clone(&term), event_proxy, pty, false, false)
            .context("creating pty event loop")?;
        let sender = pty_loop.channel();
        let notifier = Notifier(sender.clone());
        let _handle = pty_loop.spawn();

        Ok(Self {
            id,
            term,
            notifier,
            sender,
            title: None,
            custom_title: None,
            base_title: launch.title.clone(),
            exited: false,
            size,
            view: Default::default(),
        })
    }

    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().or(self.title.as_deref()).unwrap_or(&self.base_title)
    }

    /// Write bytes to the child process.
    pub fn write<B: Into<Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
    }

    pub fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        self.notifier.on_resize(size.into());
    }

    /// Apply new terminal options (cursor style, scrollback) live.
    pub fn set_options(&self, config: TermConfig) {
        self.term.lock().set_options(config);
    }

    pub fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}
