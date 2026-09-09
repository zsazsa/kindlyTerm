//! One terminal session: the alacritty grid/parser plus either an
//! in-process PTY (local backend) or a connection to a detached PTY host
//! (remote backend, see `host.rs` / `session.rs`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use alacritty_terminal::vte::ansi::Processor;

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
/// Reserved `UserEvent.tab` value: a config file changed on disk.
pub const SYS_CONFIG_CHANGED: TabId = u64::MAX - 2;
/// Reserved `UserEvent.tab` value: control-socket requests are queued.
pub const SYS_CONTROL: TabId = u64::MAX - 3;
/// Reserved `UserEvent.tab` value: another launch asked for a window.
pub const SYS_INSTANCE: TabId = u64::MAX - 4;

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
    /// Remote terminals: the host answers device queries, the UI must not.
    remote: bool,
    /// Set while catching up (replay or snapshot): the output is old, so
    /// no query answers, clipboard writes, or bells.
    quiet: Arc<AtomicBool>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        if self.remote && matches!(event, Event::PtyWrite(_) | Event::TextAreaSizeRequest(_)) {
            return;
        }
        if self.quiet.load(Ordering::Relaxed)
            && matches!(event, Event::ColorRequest(..) | Event::ClipboardStore(..) | Event::ClipboardLoad(..) | Event::Bell | Event::Wakeup)
        {
            return;
        }
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
    /// Saved-command name this was launched from, if any.
    pub shortcut: Option<String>,
}

enum Backend {
    /// PTY and I/O thread inside this process.
    Local { notifier: Notifier, sender: EventLoopSender },
    /// Detached host reached over a Unix socket.
    Remote { tx: Arc<Mutex<UnixStream>> },
}

/// Facts a host reports about itself when the UI attaches.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HostInfo {
    pub title: Option<String>,
    pub program: String,
    pub cwd: Option<String>,
    pub pid: u32,
}

pub struct Terminal {
    pub id: TabId,
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    backend: Backend,
    /// Detached session id when hosted; None for an in-process shell.
    pub session: Option<String>,
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
    /// Saved-command name this was launched from, if any (for persistence).
    pub shortcut: Option<String>,
    /// When the program last produced output (for the inactivity monitor).
    pub last_output: std::time::Instant,
    /// Set by the monitor once the quiet threshold passed; cleared by output.
    pub quiet_alert: bool,
    /// When an agent (control API) last typed here: the frame glows briefly
    /// so tool input is visible even on a small or unfocused terminal.
    pub agent_touch: Option<std::time::Instant>,
}

/// How long the agent-input glow lasts.
pub const AGENT_GLOW_MS: u64 = 1400;

impl Terminal {
    pub fn spawn(
        id: TabId,
        proxy: EventLoopProxy<UserEvent>,
        launch: &Launch,
        size: GridSize,
        config: TermConfig,
    ) -> Result<Self> {
        let event_proxy = EventProxy { tab: id, proxy, remote: false, quiet: Arc::new(AtomicBool::new(false)) };

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
            backend: Backend::Local { notifier, sender },
            session: None,
            title: None,
            custom_title: None,
            base_title: launch.title.clone(),
            exited: false,
            size,
            view: Default::default(),
            shortcut: launch.shortcut.clone(),
            last_output: std::time::Instant::now(),
            quiet_alert: false,
            agent_touch: None,
        })
    }

    /// Start a detached host for `launch` and attach to it. The shell keeps
    /// running when this process exits; `session` names it for later.
    pub fn spawn_hosted(
        id: TabId,
        proxy: EventLoopProxy<UserEvent>,
        launch: &Launch,
        size: GridSize,
        config: TermConfig,
    ) -> Result<Self> {
        let session = crate::session::new_id();
        crate::session::ensure_dir().context("session dir")?;
        let exe = std::env::current_exe().context("current exe")?;
        let log = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(crate::session::log_path(&session)).ok();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--host")
            .arg(&session)
            .arg("--size")
            .arg(format!("{}x{}x{}x{}", size.cols, size.rows, size.cell_width, size.cell_height))
            .arg("--history")
            .arg(config.scrolling_history.to_string());
        if let Some(cwd) = &launch.cwd {
            cmd.arg("--cwd").arg(cwd);
        }
        cmd.arg("--").arg(&launch.program).args(&launch.args);
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
        match log {
            Some(f) => {
                cmd.stderr(f);
            }
            None => {
                cmd.stderr(std::process::Stdio::null());
            }
        }
        // The host binds its socket, forks, and the parent returns at once.
        let status = cmd.status().context("starting session host")?;
        if !status.success() {
            let why = std::fs::read_to_string(crate::session::log_path(&session)).unwrap_or_default();
            anyhow::bail!("session host failed to start: {}", why.lines().last().unwrap_or("no details"));
        }
        let mut t = Self::attach(id, proxy, &session, size, config, launch.title.clone())?;
        t.shortcut = launch.shortcut.clone();
        Ok(t)
    }

    /// Attach to an existing host. A fresh grid is caught up by replay or
    /// snapshot before the first frame is shown.
    pub fn attach(
        id: TabId,
        proxy: EventLoopProxy<UserEvent>,
        session: &str,
        size: GridSize,
        config: TermConfig,
        fallback_title: String,
    ) -> Result<Self> {
        let path = crate::session::socket_path(session);
        let mut stream = UnixStream::connect(&path).with_context(|| format!("connecting to session {session}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let hello = crate::session::ToHost::Hello {
            last_seq: crate::session::SEQ_NONE,
            cols: size.cols as u16,
            rows: size.rows as u16,
            cell_w: size.cell_width,
            cell_h: size.cell_height,
        };
        stream.write_all(&hello.encode()).context("hello")?;
        let info = match crate::session::read_from_host(&mut stream) {
            Ok(Some(crate::session::FromHost::Attached { title, program, cwd, pid, .. })) => HostInfo { title, program, cwd, pid },
            Ok(other) => anyhow::bail!("unexpected answer from session host: {other:?}"),
            Err(e) => return Err(e).context("reading from session host"),
        };
        stream.set_read_timeout(None).ok();

        let quiet = Arc::new(AtomicBool::new(true));
        let event_proxy = EventProxy { tab: id, proxy: proxy.clone(), remote: true, quiet: quiet.clone() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, event_proxy)));
        let rx = stream.try_clone().context("dup socket")?;
        let tx = Arc::new(Mutex::new(stream));
        let base_title = std::path::Path::new(&info.program).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(fallback_title);

        {
            let term = term.clone();
            let quiet = quiet.clone();
            let seq = Arc::new(AtomicU64::new(0));
            std::thread::Builder::new().name(format!("session-{session}")).spawn(move || {
                let mut rx = rx;
                let mut parser: Processor = Processor::default();
                loop {
                    match crate::session::read_from_host(&mut rx) {
                        Ok(Some(crate::session::FromHost::Output { seq: s, bytes })) => {
                            {
                                let mut t = term.lock();
                                parser.advance(&mut *t, &bytes);
                            }
                            seq.store(s + bytes.len() as u64, Ordering::Relaxed);
                            if !quiet.load(Ordering::Relaxed) {
                                let _ = proxy.send_event(UserEvent { tab: id, event: Event::Wakeup });
                            }
                        }
                        Ok(Some(crate::session::FromHost::Live { seq: s })) => {
                            seq.store(s, Ordering::Relaxed);
                            quiet.store(false, Ordering::Relaxed);
                            let _ = proxy.send_event(UserEvent { tab: id, event: Event::Wakeup });
                        }
                        Ok(Some(crate::session::FromHost::Exited { .. })) => {
                            let _ = proxy.send_event(UserEvent { tab: id, event: Event::Exit });
                            break;
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => {
                            // Host gone (or we detached). A dead host takes
                            // its shell with it, so treat it as an exit.
                            let _ = proxy.send_event(UserEvent { tab: id, event: Event::Exit });
                            break;
                        }
                    }
                }
            })?;
        }

        Ok(Self {
            id,
            term,
            backend: Backend::Remote { tx },
            session: Some(session.to_string()),
            title: info.title.clone(),
            custom_title: None,
            base_title,
            exited: false,
            size,
            view: Default::default(),
            shortcut: None,
            last_output: std::time::Instant::now(),
            quiet_alert: false,
            agent_touch: None,
        })
    }

    /// Strength of the agent-input glow right now, 1.0 fading to 0.0.
    pub fn agent_glow(&self) -> f32 {
        match self.agent_touch {
            Some(at) => {
                let t = at.elapsed().as_millis() as f32 / AGENT_GLOW_MS as f32;
                if t >= 1.0 { 0.0 } else { (1.0 - t) * (1.0 - t) }
            }
            None => 0.0,
        }
    }

    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().or(self.title.as_deref()).unwrap_or(&self.base_title)
    }

    /// Write bytes to the child process.
    pub fn write<B: Into<Cow<'static, [u8]>>>(&self, bytes: B) {
        match &self.backend {
            Backend::Local { notifier, .. } => notifier.notify(bytes),
            Backend::Remote { tx } => {
                let b: Cow<'static, [u8]> = bytes.into();
                let frame = crate::session::ToHost::Input(b.into_owned()).encode();
                if let Ok(mut s) = tx.lock() {
                    let _ = s.write_all(&frame);
                }
            }
        }
    }

    pub fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        match &mut self.backend {
            Backend::Local { notifier, .. } => notifier.on_resize(size.into()),
            Backend::Remote { tx } => {
                let frame = crate::session::ToHost::Resize { cols: size.cols as u16, rows: size.rows as u16, cell_w: size.cell_width, cell_h: size.cell_height }.encode();
                if let Ok(mut s) = tx.lock() {
                    let _ = s.write_all(&frame);
                }
            }
        }
    }

    /// End the shell for good: hang it up (hosted sessions die with it).
    pub fn kill(&self) {
        match &self.backend {
            Backend::Local { sender, .. } => {
                let _ = sender.send(Msg::Shutdown);
            }
            Backend::Remote { tx } => {
                if let Ok(mut s) = tx.lock() {
                    let _ = s.write_all(&crate::session::ToHost::Kill.encode());
                }
            }
        }
    }

    /// Apply new terminal options (cursor style, scrollback) live.
    pub fn set_options(&self, config: TermConfig) {
        self.term.lock().set_options(config);
    }

    pub fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    /// Let go of the terminal. In-process shells end; hosted sessions keep
    /// running detached.
    pub fn shutdown(&self) {
        match &self.backend {
            Backend::Local { sender, .. } => {
                let _ = sender.send(Msg::Shutdown);
            }
            Backend::Remote { tx } => {
                if let Ok(s) = tx.lock() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}
