//! PTY host: `kindlyterm --host <id> …` runs one shell detached from any
//! window and serves it over a Unix socket (see `session.rs`).
//!
//! The host keeps a bounded ring of raw output (for byte-exact replay when
//! the UI reconnects) and a headless alacritty `Term` (for a snapshot when
//! the ring no longer reaches back far enough). It answers terminal
//! queries itself so programs never hang while no window is attached.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor};
use anyhow::{Context, Result, bail};

use crate::session::{self, FromHost, SEQ_NONE, ToHost};
use crate::terminal::GridSize;

/// Raw output kept for replay.
const RING_BYTES: usize = 8 * 1024 * 1024;
/// Output frames are chunked so a burst never becomes one giant frame.
const CHUNK: usize = 256 * 1024;

pub struct HostArgs {
    pub id: String,
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub history: usize,
    pub cwd: Option<String>,
    pub program: String,
    pub args: Vec<String>,
}

impl HostArgs {
    /// Parse everything after `--host`.
    pub fn parse(mut argv: std::env::Args) -> Result<Self> {
        let id = argv.next().context("--host needs a session id")?;
        if !session::valid_id(&id) {
            bail!("bad session id");
        }
        let mut h = HostArgs { id, cols: 80, rows: 24, cell_w: 8, cell_h: 16, history: 10_000, cwd: None, program: String::new(), args: Vec::new() };
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--size" => {
                    let v = argv.next().context("--size needs COLSxROWSxCWxCH")?;
                    let p: Vec<u16> = v.split('x').filter_map(|s| s.parse().ok()).collect();
                    if p.len() != 4 {
                        bail!("bad --size");
                    }
                    (h.cols, h.rows, h.cell_w, h.cell_h) = (p[0].max(2), p[1].max(1), p[2].max(1), p[3].max(1));
                }
                "--history" => h.history = argv.next().and_then(|s| s.parse().ok()).context("--history needs a number")?,
                "--cwd" => h.cwd = Some(argv.next().context("--cwd needs a path")?),
                "--" => {
                    h.program = argv.next().context("no program after --")?;
                    h.args = argv.collect();
                    break;
                }
                other => bail!("unknown host option {other}"),
            }
        }
        if h.program.is_empty() {
            bail!("no program given");
        }
        Ok(h)
    }
}

// ---------------------------------------------------------------------------
// Output ring
// ---------------------------------------------------------------------------

struct Ring {
    buf: VecDeque<u8>,
    /// Sequence number of the byte after the last one pushed.
    end: u64,
}

impl Ring {
    fn new() -> Self {
        Self { buf: VecDeque::with_capacity(64 * 1024), end: 0 }
    }
    fn base(&self) -> u64 {
        self.end - self.buf.len() as u64
    }
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
        self.end += bytes.len() as u64;
        if self.buf.len() > RING_BYTES {
            let drop = self.buf.len() - RING_BYTES;
            self.buf.drain(..drop);
        }
    }
    /// Bytes from `seq` onwards, if the ring still holds them.
    fn since(&self, seq: u64) -> Option<Vec<u8>> {
        if seq < self.base() || seq > self.end {
            return None;
        }
        let skip = (seq - self.base()) as usize;
        Some(self.buf.iter().skip(skip).copied().collect())
    }
}

// ---------------------------------------------------------------------------
// Headless terminal listener
// ---------------------------------------------------------------------------

/// What the headless `Term` reports back. It answers device queries on the
/// PTY directly; colour queries are left to an attached UI (it knows the
/// theme) and are dropped when nothing is attached.
struct Listener {
    pty: Mutex<File>,
    title: Mutex<Option<String>>,
    size: Mutex<WindowSize>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(t) => *self.title.lock().unwrap() = Some(t),
            Event::ResetTitle => *self.title.lock().unwrap() = None,
            Event::PtyWrite(s) => {
                let _ = self.pty.lock().unwrap().write_all(s.as_bytes());
            }
            Event::TextAreaSizeRequest(f) => {
                let s = f(*self.size.lock().unwrap());
                let _ = self.pty.lock().unwrap().write_all(s.as_bytes());
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Shared host state
// ---------------------------------------------------------------------------

struct Client {
    id: u64,
    tx: Arc<Mutex<UnixStream>>,
}

struct Host {
    id: String,
    program: String,
    cwd: Option<String>,
    child_pid: i32,
    listener: Arc<Listener>,
    term: FairMutex<Term<SharedListener>>,
    parser: Mutex<Processor>,
    /// Lock order: `ring` before `term`. The ring lock also guards client
    /// registration so nobody misses or duplicates output.
    ring: Mutex<Ring>,
    clients: Mutex<Vec<Client>>,
    next_client: Mutex<u64>,
    master_fd: i32,
    default_cursor: CursorStyle,
    exiting: AtomicBool,
}

/// `Term` owns its listener by value; hand it a shared handle.
#[derive(Clone)]
struct SharedListener(Arc<Listener>);

impl EventListener for SharedListener {
    fn send_event(&self, event: Event) {
        self.0.send_event(event)
    }
}

impl Host {
    fn resize(&self, cols: u16, rows: u16, cell_w: u16, cell_h: u16) {
        let ws = WindowSize { num_cols: cols.max(2), num_lines: rows.max(1), cell_width: cell_w.max(1), cell_height: cell_h.max(1) };
        {
            let cur = *self.listener.size.lock().unwrap();
            if cur.num_cols == ws.num_cols && cur.num_lines == ws.num_lines && cur.cell_width == ws.cell_width && cur.cell_height == ws.cell_height {
                return;
            }
        }
        let g = GridSize { cols: ws.num_cols as usize, rows: ws.num_lines as usize, cell_width: ws.cell_width, cell_height: ws.cell_height };
        self.term.lock().resize(g);
        *self.listener.size.lock().unwrap() = ws;
        let win = libc::winsize { ws_row: ws.num_lines, ws_col: ws.num_cols, ws_xpixel: ws.num_cols * ws.cell_width, ws_ypixel: ws.num_lines * ws.cell_height };
        unsafe {
            libc::ioctl(self.master_fd, libc::TIOCSWINSZ, &win as *const libc::winsize);
        }
    }

    fn info(&self, seq: u64, replay: bool) -> FromHost {
        let title = self.listener.title.lock().unwrap().clone();
        FromHost::Attached { seq, replay, title, program: self.program.clone(), cwd: self.cwd.clone(), pid: self.child_pid as u32 }
    }

    fn broadcast(&self, frame: &[u8]) {
        let list: Vec<(u64, Arc<Mutex<UnixStream>>)> = self.clients.lock().unwrap().iter().map(|c| (c.id, c.tx.clone())).collect();
        let mut dead = Vec::new();
        for (id, tx) in list {
            if tx.lock().unwrap().write_all(frame).is_err() {
                dead.push(id);
            }
        }
        if !dead.is_empty() {
            self.clients.lock().unwrap().retain(|c| !dead.contains(&c.id));
        }
    }

    /// Feed PTY output: ring, headless term, then every attached client.
    fn on_output(&self, bytes: &[u8]) {
        let (seq, list) = {
            let mut ring = self.ring.lock().unwrap();
            let seq = ring.end;
            ring.push(bytes);
            {
                let mut term = self.term.lock();
                self.parser.lock().unwrap().advance(&mut *term, bytes);
            }
            let list: Vec<(u64, Arc<Mutex<UnixStream>>)> = self.clients.lock().unwrap().iter().map(|c| (c.id, c.tx.clone())).collect();
            (seq, list)
        };
        if list.is_empty() {
            return;
        }
        let frame = FromHost::Output { seq, bytes: bytes.to_vec() }.encode();
        let mut dead = Vec::new();
        for (id, tx) in list {
            if tx.lock().unwrap().write_all(&frame).is_err() {
                dead.push(id);
            }
        }
        if !dead.is_empty() {
            self.clients.lock().unwrap().retain(|c| !dead.contains(&c.id));
        }
    }

    /// Serve one UI connection until it hangs up.
    fn serve(self: &Arc<Self>, stream: UnixStream) {
        let mut rx = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        };
        let tx = Arc::new(Mutex::new(stream));
        // First frame must be Hello (attach) or Query (who are you?).
        let hello = match session::read_to_host(&mut rx) {
            Ok(Some(ToHost::Hello { last_seq, cols, rows, cell_w, cell_h })) => (last_seq, cols, rows, cell_w, cell_h),
            Ok(Some(ToHost::Query)) => {
                let _ = tx.lock().unwrap().write_all(&self.info(0, false).encode());
                return;
            }
            Ok(_) | Err(_) => return,
        };
        let (last_seq, cols, rows, cell_w, cell_h) = hello;
        let id = {
            let mut n = self.next_client.lock().unwrap();
            *n += 1;
            *n
        };
        // A byte-exact replay only makes sense at the size the bytes were
        // produced for; at any other size the host resizes first (programs
        // get SIGWINCH and redraw) and the client gets a snapshot.
        let same_size = {
            let cur = *self.listener.size.lock().unwrap();
            cur.num_cols == cols.max(2) && cur.num_lines == rows.max(1)
        };
        self.resize(cols, rows, cell_w, cell_h);
        {
            // Under the ring lock: decide replay vs snapshot, register the
            // client, and take its send lock so nothing can be written to it
            // before the catch-up data.
            let ring = self.ring.lock().unwrap();
            let end = ring.end;
            let replay = if !same_size {
                None
            } else if last_seq == SEQ_NONE {
                ring.since(0)
            } else {
                ring.since(last_seq)
            };
            let (from, bytes, is_replay) = match replay {
                Some(b) => (if last_seq == SEQ_NONE { 0 } else { last_seq }, b, true),
                None => (end, self.snapshot(), false),
            };
            let mut w = tx.lock().unwrap();
            self.clients.lock().unwrap().push(Client { id, tx: tx.clone() });
            drop(ring);
            let mut ok = w.write_all(&self.info(from, is_replay).encode()).is_ok();
            let mut seq = from;
            for chunk in bytes.chunks(CHUNK) {
                if !ok {
                    break;
                }
                ok = w.write_all(&FromHost::Output { seq, bytes: chunk.to_vec() }.encode()).is_ok();
                seq += chunk.len() as u64;
            }
            if ok {
                ok = w.write_all(&FromHost::Live { seq: end }.encode()).is_ok();
            }
            drop(w);
            if !ok {
                self.clients.lock().unwrap().retain(|c| c.id != id);
                return;
            }
        }
        log::info!("client {id} attached (last_seq {})", if last_seq == SEQ_NONE { "none".to_string() } else { last_seq.to_string() });
        loop {
            match session::read_to_host(&mut rx) {
                Ok(Some(ToHost::Input(b))) => {
                    let _ = self.listener.pty.lock().unwrap().write_all(&b);
                }
                Ok(Some(ToHost::Resize { cols, rows, cell_w, cell_h })) => self.resize(cols, rows, cell_w, cell_h),
                Ok(Some(ToHost::Ping)) => {
                    let _ = tx.lock().unwrap().write_all(&FromHost::Pong.encode());
                }
                Ok(Some(ToHost::Kill)) => {
                    log::info!("client {id} asked to kill the session");
                    unsafe {
                        libc::kill(self.child_pid, libc::SIGHUP);
                    }
                    // The reader thread notices the exit and finishes up. If
                    // the shell ignores SIGHUP, follow with SIGKILL.
                    let pid = self.child_pid;
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                        }
                    });
                }
                Ok(Some(ToHost::Hello { .. })) | Ok(Some(ToHost::Query)) => break,
                Ok(None) | Err(_) => break,
            }
        }
        self.clients.lock().unwrap().retain(|c| c.id != id);
        log::info!("client {id} detached");
    }

    /// The headless terminal's scrollback, screen, cursor and modes as
    /// escape sequences that recreate them in a fresh terminal of the same
    /// size. This is what tmux does on attach: no alacritty internals cross
    /// the wire.
    fn snapshot(&self) -> Vec<u8> {
        let mut term = self.term.lock();
        let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
        let cols = term.grid().columns();
        let rows = term.grid().screen_lines();
        let mode = *term.mode();
        let alt = mode.contains(TermMode::ALT_SCREEN);

        // Modes that programs set and expect to persist.
        if !mode.contains(TermMode::LINE_WRAP) {
            out.extend_from_slice(b"\x1b[?7l");
        }

        // Primary screen with its scrollback, written top to bottom so the
        // history scrolls into place. While the alt screen is up the
        // primary sits in the inactive grid: swap it in for a moment (the
        // alt grid is cloned first because swapping back resets it).
        let alt_grid = alt.then(|| term.grid().clone());
        if alt {
            term.swap_alt();
        }
        {
            let grid = term.grid();
            let history = grid.history_size();
            let mut sgr = Sgr::default();
            let first = -(history as i32);
            let last = rows as i32 - 1;
            for l in first..=last {
                let row = &grid[Line(l)];
                let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
                emit_line(&mut out, row, cols, &mut sgr, wrapped);
                if !wrapped && l != last {
                    out.extend_from_slice(b"\x1b[0m\r\n");
                    sgr = Sgr::default();
                }
            }
            out.extend_from_slice(b"\x1b[0m");
            let c = grid.cursor.point;
            out.extend_from_slice(format!("\x1b[{};{}H", c.line.0 + 1, c.column.0 + 1).as_bytes());
        }
        if let Some(alt_grid) = alt_grid {
            term.swap_alt();
            *term.grid_mut() = alt_grid;
            // Enter the alt screen (saving the primary cursor) and paint it
            // in place.
            out.extend_from_slice(b"\x1b[?1049h");
            let grid = term.grid();
            let mut sgr = Sgr::default();
            for line in 0..rows {
                out.extend_from_slice(format!("\x1b[{};1H", line + 1).as_bytes());
                emit_line(&mut out, &grid[Line(line as i32)], cols, &mut sgr, false);
            }
            out.extend_from_slice(b"\x1b[0m");
            let c = grid.cursor.point;
            out.extend_from_slice(format!("\x1b[{};{}H", c.line.0 + 1, c.column.0 + 1).as_bytes());
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.extend_from_slice(b"\x1b[?25l");
        }
        let style = term.cursor_style();
        if style != self.default_cursor {
            let n = match (style.shape, style.blinking) {
                (CursorShape::Block, true) => 1,
                (CursorShape::Block, false) => 2,
                (CursorShape::Underline, true) => 3,
                (CursorShape::Underline, false) => 4,
                (CursorShape::Beam, true) => 5,
                (CursorShape::Beam, false) => 6,
                _ => 0,
            };
            out.extend_from_slice(format!("\x1b[{n} q").as_bytes());
        }
        // Input modes.
        let set = |out: &mut Vec<u8>, on: bool, seq: &str| {
            if on {
                out.extend_from_slice(seq.as_bytes());
            }
        };
        set(&mut out, mode.contains(TermMode::APP_CURSOR), "\x1b[?1h");
        set(&mut out, mode.contains(TermMode::APP_KEYPAD), "\x1b=");
        set(&mut out, mode.contains(TermMode::BRACKETED_PASTE), "\x1b[?2004h");
        set(&mut out, mode.contains(TermMode::FOCUS_IN_OUT), "\x1b[?1004h");
        set(&mut out, mode.contains(TermMode::MOUSE_REPORT_CLICK), "\x1b[?1000h");
        set(&mut out, mode.contains(TermMode::MOUSE_DRAG), "\x1b[?1002h");
        set(&mut out, mode.contains(TermMode::MOUSE_MOTION), "\x1b[?1003h");
        set(&mut out, mode.contains(TermMode::UTF8_MOUSE), "\x1b[?1005h");
        set(&mut out, mode.contains(TermMode::SGR_MOUSE), "\x1b[?1006h");
        set(&mut out, mode.contains(TermMode::ALTERNATE_SCROLL), "\x1b[?1007h");
        set(&mut out, mode.contains(TermMode::LINE_FEED_NEW_LINE), "\x1b[20h");
        set(&mut out, mode.contains(TermMode::INSERT), "\x1b[4h");
        set(&mut out, mode.contains(TermMode::ORIGIN), "\x1b[?6h");
        // Kitty keyboard flags.
        let mut kitty = 0u32;
        for (m, bit) in [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ] {
            if mode.contains(m) {
                kitty |= bit;
            }
        }
        if kitty != 0 {
            out.extend_from_slice(format!("\x1b[>{kitty}u").as_bytes());
        }
        // Title.
        if let Some(t) = self.listener.title.lock().unwrap().as_deref() {
            out.extend_from_slice(b"\x1b]2;");
            out.extend_from_slice(t.as_bytes());
            out.extend_from_slice(b"\x07");
        }
        out
    }
}

/// Current SGR state while serialising, so attributes are only emitted
/// when they change.
#[derive(Clone, PartialEq, Eq)]
struct Sgr {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Default for Sgr {
    fn default() -> Self {
        Self { fg: Color::Named(NamedColor::Foreground), bg: Color::Named(NamedColor::Background), flags: Flags::empty() }
    }
}

const STYLE_FLAGS: Flags = Flags::INVERSE
    .union(Flags::BOLD)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DIM)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT)
    .union(Flags::DOUBLE_UNDERLINE)
    .union(Flags::UNDERCURL)
    .union(Flags::DOTTED_UNDERLINE)
    .union(Flags::DASHED_UNDERLINE);

fn color_params(c: Color, fg: bool) -> String {
    let (base, ext) = if fg { (30, 38) } else { (40, 48) };
    match c {
        Color::Named(n) => {
            let i = n as usize;
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground if fg => "39".into(),
                NamedColor::Background if !fg => "49".into(),
                _ if i < 8 => format!("{}", base + i),
                _ if i < 16 => format!("{}", base + 60 + (i - 8)),
                _ => if fg { "39".into() } else { "49".into() },
            }
        }
        Color::Indexed(i) => format!("{ext};5;{i}"),
        Color::Spec(rgb) => format!("{ext};2;{};{};{}", rgb.r, rgb.g, rgb.b),
    }
}

/// One grid row. `keep_full` (a wrapped line) writes every column so the
/// receiving terminal wraps at exactly the same place; otherwise trailing
/// plain blanks are trimmed.
fn emit_line(out: &mut Vec<u8>, row: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>, cols: usize, sgr: &mut Sgr, keep_full: bool) {
    let default_bg = Color::Named(NamedColor::Background);
    // Last column worth emitting.
    let mut last = cols;
    if !keep_full {
        while last > 0 {
            let c = &row[Column(last - 1)];
            let plain = c.c == ' ' && c.bg == default_bg && !c.flags.intersects(Flags::INVERSE | Flags::UNDERLINE | Flags::STRIKEOUT | Flags::ALL_UNDERLINES) && c.zerowidth().is_none();
            if !plain {
                break;
            }
            last -= 1;
        }
    }
    let mut col = 0;
    while col < last {
        let cell = &row[Column(col)];
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            col += 1;
            continue;
        }
        let want = Sgr { fg: cell.fg, bg: cell.bg, flags: cell.flags & STYLE_FLAGS };
        if want != *sgr {
            let mut params: Vec<String> = vec!["0".into()];
            let f = want.flags;
            if f.contains(Flags::BOLD) {
                params.push("1".into());
            }
            if f.contains(Flags::DIM) {
                params.push("2".into());
            }
            if f.contains(Flags::ITALIC) {
                params.push("3".into());
            }
            if f.contains(Flags::UNDERLINE) {
                params.push("4".into());
            } else if f.contains(Flags::DOUBLE_UNDERLINE) {
                params.push("4:2".into());
            } else if f.contains(Flags::UNDERCURL) {
                params.push("4:3".into());
            } else if f.contains(Flags::DOTTED_UNDERLINE) {
                params.push("4:4".into());
            } else if f.contains(Flags::DASHED_UNDERLINE) {
                params.push("4:5".into());
            }
            if f.contains(Flags::INVERSE) {
                params.push("7".into());
            }
            if f.contains(Flags::HIDDEN) {
                params.push("8".into());
            }
            if f.contains(Flags::STRIKEOUT) {
                params.push("9".into());
            }
            let fgp = color_params(want.fg, true);
            if fgp != "39" {
                params.push(fgp);
            }
            let bgp = color_params(want.bg, false);
            if bgp != "49" {
                params.push(bgp);
            }
            out.extend_from_slice(format!("\x1b[{}m", params.join(";")).as_bytes());
            *sgr = want;
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
        if let Some(zw) = cell.zerowidth() {
            for z in zw {
                out.extend_from_slice(z.encode_utf8(&mut buf).as_bytes());
            }
        }
        col += 1;
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(args: HostArgs) -> Result<()> {
    let dir = session::ensure_dir()?;
    let sock_path = session::socket_path(&args.id);
    if sock_path.exists() {
        // A stale socket from a dead host with the same id; ids are random
        // so this is only a crash leftover.
        std::fs::remove_file(&sock_path).ok();
    }
    let listener = UnixListener::bind(&sock_path).with_context(|| format!("binding {}", sock_path.display()))?;
    std::fs::set_permissions(&sock_path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).ok();

    // Detach: the socket exists and is listening, so the UI can connect at
    // once. Fork and let the parent return so the UI's spawn() completes
    // and reaps it; the child becomes a session leader with no controlling
    // terminal and lives on after every window is gone.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            bail!("fork failed");
        }
        if pid > 0 {
            libc::_exit(0);
        }
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        // stdin/stdout are not used; keep stderr for the log file the UI
        // handed us.
        if let Ok(null) = File::open("/dev/null") {
            libc::dup2(null.as_raw_fd(), 0);
        }
        if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
            libc::dup2(null.as_raw_fd(), 1);
        }
    }
    log::info!("host {} starting in {}", args.id, dir.display());

    let mut env = std::collections::HashMap::new();
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    env.insert("TERM_PROGRAM".to_string(), "kindlyTerm".to_string());
    env.insert("KINDLYTERM_SESSION".to_string(), args.id.clone());
    let options = tty::Options {
        shell: Some(tty::Shell::new(args.program.clone(), args.args.clone())),
        working_directory: args.cwd.clone().map(Into::into),
        drain_on_exit: false,
        env,
    };
    let ws = WindowSize { num_cols: args.cols, num_lines: args.rows, cell_width: args.cell_w, cell_height: args.cell_h };
    let pty = tty::new(&options, ws, 0).context("spawning pty")?;
    let master_fd = pty.file().as_raw_fd();
    // alacritty sets the master non-blocking for its poller; this host uses
    // plain blocking threads.
    unsafe {
        let fl = libc::fcntl(master_fd, libc::F_GETFL, 0);
        libc::fcntl(master_fd, libc::F_SETFL, fl & !libc::O_NONBLOCK);
    }
    let child_pid = pty.child().id() as i32;
    let writer = pty.file().try_clone().context("dup pty")?;
    let mut reader = pty.file().try_clone().context("dup pty")?;

    let default_cursor = CursorStyle { shape: CursorShape::Block, blinking: false };
    let listener_state = Arc::new(Listener { pty: Mutex::new(writer), title: Mutex::new(None), size: Mutex::new(ws) });
    let grid = GridSize { cols: args.cols as usize, rows: args.rows as usize, cell_width: args.cell_w, cell_height: args.cell_h };
    let term = Term::new(TermConfig { scrolling_history: args.history, default_cursor_style: default_cursor, ..TermConfig::default() }, &grid, SharedListener(listener_state.clone()));
    let host = Arc::new(Host {
        id: args.id.clone(),
        program: args.program.clone(),
        cwd: args.cwd.clone(),
        child_pid,
        listener: listener_state,
        term: FairMutex::new(term),
        parser: Mutex::new(Processor::default()),
        ring: Mutex::new(Ring::new()),
        clients: Mutex::new(Vec::new()),
        next_client: Mutex::new(0),
        master_fd,
        default_cursor,
        exiting: AtomicBool::new(false),
    });

    // PTY reader: the only producer of output.
    {
        let host = host.clone();
        let sock_path = sock_path.clone();
        std::thread::Builder::new().name("pty-read".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => host.on_output(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break, // EIO: slave side closed, the shell is gone.
                }
            }
            host.exiting.store(true, Ordering::SeqCst);
            // Reap the shell for its exit code.
            let mut status: libc::c_int = 0;
            let code = unsafe {
                if libc::waitpid(host.child_pid, &mut status, 0) == host.child_pid {
                    if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { 128 + libc::WTERMSIG(status) }
                } else {
                    -1
                }
            };
            log::info!("shell exited with {code}; host {} shutting down", host.id);
            host.broadcast(&FromHost::Exited { code }.encode());
            let _ = std::fs::remove_file(&sock_path);
            let _ = std::fs::remove_file(session::log_path(&host.id));
            // Give the Exited frame a moment to flush, then go.
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::process::exit(0);
        })?;
    }
    // Keep the Pty alive (its Drop would SIGHUP the shell) for the life of
    // the process without letting it be dropped early.
    std::mem::forget(pty);

    for conn in listener.incoming() {
        if host.exiting.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = conn else { continue };
        let host = host.clone();
        std::thread::Builder::new().name("client".into()).spawn(move || host.serve(stream))?;
    }
    Ok(())
}

/// `kindlyterm --sessions [--prune]`: list live hosts, drop dead sockets.
pub fn sessions_cli(prune: bool) -> Result<()> {
    let ids = session::list_ids();
    if ids.is_empty() {
        println!("no sessions in {}", session::dir().display());
        return Ok(());
    }
    for id in ids {
        let path = session::socket_path(&id);
        match UnixStream::connect(&path) {
            Ok(mut s) => {
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let _ = s.write_all(&ToHost::Query.encode());
                let info = match session::read_from_host(&mut s) {
                    Ok(Some(FromHost::Attached { program, cwd, pid, title, .. })) => {
                        format!("pid {pid}  {program}{}{}", cwd.map(|c| format!("  cwd {c}")).unwrap_or_default(), title.map(|t| format!("  \"{t}\"")).unwrap_or_default())
                    }
                    _ => "live".to_string(),
                };
                println!("{id}  {info}");
            }
            Err(_) => {
                if prune {
                    session::remove_files(&id);
                    println!("{id}  dead (removed)");
                } else {
                    println!("{id}  dead (run with --prune to remove)");
                }
            }
        }
    }
    Ok(())
}
