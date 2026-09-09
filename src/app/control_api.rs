//! Handlers for the control socket (`control.rs`): what the MCP tools do
//! inside the running app. Every call runs on the UI thread. Anything that
//! types, closes, or moves something leaves a notice in the status bar so
//! the user always sees a tool acting.

use super::*;
use crate::canvas::{Group, GroupId};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// One unit of timed tool input.
pub(super) enum DripStep {
    /// A typed character (bytes to send) that lights the typing trail.
    Char(char, Vec<u8>),
    /// A Backspace (true) or Delete (false) repeat that chomps.
    Eat(bool, Vec<u8>),
}

/// Tool input fed to a terminal over time so the user can watch it: typed
/// text at a typing speed, or a key held down. The tool call returns at
/// once; the app plays the queue on its timer.
pub(super) struct Drip {
    pub wi: usize,
    pub tab: TabId,
    pub steps: VecDeque<DripStep>,
    pub next: Instant,
    pub interval: std::time::Duration,
}

type R = Result<Value, String>;

fn arg_u64(p: &Value, k: &str) -> Option<u64> {
    p.get(k).and_then(|v| v.as_u64())
}
fn arg_f32(p: &Value, k: &str) -> Option<f32> {
    p.get(k).and_then(|v| v.as_f64()).map(|v| v as f32)
}
fn arg_str<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(|v| v.as_str())
}
fn need_u64(p: &Value, k: &str) -> Result<u64, String> {
    arg_u64(p, k).ok_or_else(|| format!("missing integer '{k}'"))
}
fn need_str<'a>(p: &'a Value, k: &str) -> Result<&'a str, String> {
    arg_str(p, k).ok_or_else(|| format!("missing string '{k}'"))
}

/// Rows of a grid as trimmed text, joining soft-wrapped lines.
fn grid_lines(term: &alacritty_terminal::Term<crate::terminal::EventProxy>, first: i32, last: i32) -> Vec<String> {
    let grid = term.grid();
    let cols = grid.columns();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for l in first..=last {
        let row = &grid[Line(l)];
        for c in 0..cols {
            let cell = &row[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            cur.push(cell.c);
            if let Some(zw) = cell.zerowidth() {
                cur.extend(zw.iter());
            }
        }
        if row[Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
            continue;
        }
        out.push(cur.trim_end().to_string());
        cur.clear();
    }
    if !cur.is_empty() {
        out.push(cur.trim_end().to_string());
    }
    while out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

impl App {
    /// Start or stop the control socket to match the config.
    pub(super) fn sync_control_server(&mut self) {
        let want = self.config.mcp.enabled;
        match (want, self.control.is_some()) {
            (true, false) => {
                let proxy = self.proxy.clone();
                match crate::control::Server::start(move || {
                    let _ = proxy.send_event(UserEvent { tab: crate::terminal::SYS_CONTROL, event: Event::Wakeup });
                }) {
                    Ok(s) => {
                        log::info!("control API listening on {}", crate::control::socket_path().display());
                        self.control = Some(s);
                    }
                    Err(e) => self.set_status(format!("control API failed to start: {e}")),
                }
            }
            (false, true) => {
                self.control = None;
                log::info!("control API stopped");
            }
            _ => {}
        }
    }

    /// Answer every queued request.
    pub(super) fn drain_control_requests(&mut self, event_loop: &ActiveEventLoop) {
        let reqs = match &self.control {
            Some(s) => s.drain(),
            None => return,
        };
        for r in reqs {
            let res = self.handle_control(&r.method, &r.params, event_loop);
            let _ = r.reply.send(res);
        }
        self.request_redraw_all();
    }

    /// Locate a terminal by id across windows: (window, tab index in store).
    fn find_term(&self, tab: TabId) -> Option<(usize, usize)> {
        self.wins.iter().enumerate().find_map(|(wi, w)| w.term_index(tab).map(|ti| (wi, ti)))
    }

    /// Locate an item by id: (window, canvas index).
    fn find_item(&self, id: ItemId) -> Option<(usize, usize)> {
        self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.item(id).is_some()).map(|ci| (wi, ci)))
    }

    fn find_canvas(&self, cid: CanvasId) -> Option<(usize, usize)> {
        self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.id == cid).map(|ci| (wi, ci)))
    }

    /// Make `(wi, ci)` the current window and active tab.
    fn go_to(&mut self, wi: usize, ci: usize) {
        self.cur = wi;
        if self.wins[wi].active != ci {
            self.wins[wi].active = ci;
            self.relayout();
            self.update_window_title();
        }
    }

    fn notice(&mut self, msg: String) {
        log::info!("control: {msg}");
        self.set_status(format!("tool: {msg}"));
    }

    /// Queue timed tool input. A drip for a terminal that already has one
    /// in flight starts when that one ends, so typing and key holds never
    /// interleave.
    fn push_drip(&mut self, mut d: Drip) {
        let after = self.drips.iter().filter(|x| x.wi == d.wi && x.tab == d.tab).map(|x| x.next + x.interval * x.steps.len() as u32).max();
        if let Some(t) = after {
            d.next = d.next.max(t);
        }
        self.drips.push(d);
    }

    /// Feed queued tool input to its terminal, one step per interval.
    pub(super) fn tick_drips(&mut self, now: Instant) {
        if self.drips.is_empty() {
            return;
        }
        let trail = self.effects.typing_trail.clone();
        let mut i = 0;
        while i < self.drips.len() {
            let mut done = false;
            while self.drips[i].next <= now {
                let d = &mut self.drips[i];
                let Some(step) = d.steps.pop_front() else {
                    done = true;
                    break;
                };
                d.next += d.interval;
                let (wi, tab) = (d.wi, d.tab);
                log::debug!("drip: tab {tab} step, {} left", d.steps.len());
                let Some(term) = self.wins.get_mut(wi).and_then(|w| w.term_mut(tab)) else {
                    done = true;
                    break;
                };
                term.agent_touch = Some(now);
                match step {
                    DripStep::Char(ch, bytes) => {
                        let (col, row) = term.view.cursor_anim.target();
                        term.view.fx.typed(&trail, ch, col, row);
                        term.write(bytes);
                    }
                    DripStep::Eat(left, bytes) => {
                        // A held key: chomping from the first repeat. The
                        // start is set once so the mouth keeps its rhythm.
                        let start = match term.view.cursor_anim.chomp {
                            Some((s, _, l)) if l == left => s,
                            _ => now - std::time::Duration::from_millis(super::CursorAnim::CHOMP_AFTER_MS as u64),
                        };
                        term.view.cursor_anim.chomp = Some((start, now, left));
                        term.write(bytes);
                    }
                }
                if let Some(w) = self.wins.get(wi) {
                    w.window.request_redraw();
                }
            }
            if done || self.drips[i].steps.is_empty() {
                self.drips.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Make tool input visible: the frame glows, single-line text replays
    /// the typing trail, longer text arms the paste rain. Called after the
    /// bytes went to the PTY, so it never delays the agent.
    fn show_agent_input(&mut self, wi: usize, ti: usize, text: &str) {
        let trail = self.effects.typing_trail.clone();
        let mut rain = self.effects.paste_rain.clone();
        // A tool paste is something the user did not do themselves: let a
        // big one rain for longer (up to 4x) so it registers.
        let lines = text.lines().count().max(1) as f32;
        rain.duration_ms = (rain.duration_ms as f32 * (lines / 6.0).clamp(1.0, 4.0)) as u32;
        let tab = &mut self.wins[wi].terms[ti];
        tab.agent_touch = Some(Instant::now());
        if !text.is_empty() {
            if text.contains('\n') || text.chars().count() >= rain.min_chars {
                Self::arm_paste_rain_on(tab, &rain, text);
            } else {
                let (col, row) = tab.view.cursor_anim.target();
                for (i, ch) in text.chars().enumerate() {
                    tab.view.fx.typed(&trail, ch, col + i as f32, row);
                }
            }
        }
        self.wins[wi].window.request_redraw();
    }

    fn item_json(&self, w: &Win, c: &Canvas, it: &Item) -> Value {
        let mut v = json!({
            "item_id": it.id,
            "x": it.rect.x, "y": it.rect.y, "w": it.rect.w, "h": it.rect.h,
            "pinned": it.pin.is_some(),
            "mirror": it.mirror,
            "name": it.name,
            "monitor_seconds": it.monitor,
            "focused": c.focus == Some(it.id),
        });
        match &it.kind {
            ItemKind::Terminal(t) => {
                v["kind"] = json!("terminal");
                v["terminal_id"] = json!(t);
                if let Some(term) = w.term(*t) {
                    v["title"] = json!(term.display_title());
                    // A terminal's custom name lives on the terminal until
                    // the next save copies it to the item.
                    v["name"] = json!(term.custom_title);
                    v["cols"] = json!(term.size.cols);
                    v["rows"] = json!(term.size.rows);
                    v["session"] = json!(term.session);
                    v["idle_seconds"] = json!(term.last_output.elapsed().as_secs());
                    v["quiet_alert"] = json!(term.quiet_alert);
                }
            }
            ItemKind::Image { path } => {
                v["kind"] = json!("image");
                v["path"] = json!(path);
            }
            ItemKind::Pending => v["kind"] = json!("pending"),
        }
        v
    }

    fn group_json(g: &Group) -> Value {
        json!({"group_id": g.id, "name": g.name, "x": g.rect.x, "y": g.rect.y, "w": g.rect.w, "h": g.rect.h, "members": g.members})
    }

    pub(super) fn handle_control(&mut self, method: &str, p: &Value, event_loop: &ActiveEventLoop) -> R {
        match method {
            "list_canvases" => {
                let mut wins = Vec::new();
                for (wi, w) in self.wins.iter().enumerate() {
                    let canvases: Vec<Value> = w
                        .canvases
                        .iter()
                        .enumerate()
                        .map(|(ci, c)| {
                            json!({
                                "canvas_id": c.id,
                                "index": ci,
                                "name": w.tab_title(ci),
                                "mode": if c.is_single() { "single" } else { "free" },
                                "active": w.active == ci,
                                "view": {"x": c.view.x, "y": c.view.y, "zoom": c.view.zoom,
                                         "area_w": w.layout.map(|l| l.area.w).unwrap_or(0.0), "area_h": w.layout.map(|l| l.area.h).unwrap_or(0.0)},
                                "items": c.items.iter().map(|it| self.item_json(w, c, it)).collect::<Vec<_>>(),
                                "groups": c.groups.iter().map(Self::group_json).collect::<Vec<_>>(),
                            })
                        })
                        .collect();
                    wins.push(json!({"window": wi, "current": wi == self.cur, "canvases": canvases}));
                }
                Ok(json!({"windows": wins}))
            }
            "list_terminals" => {
                let mut out = Vec::new();
                for w in &self.wins {
                    for c in &w.canvases {
                        for it in c.items.iter().filter(|i| !i.mirror) {
                            if matches!(it.kind, ItemKind::Terminal(_)) {
                                let mut v = self.item_json(w, c, it);
                                v["canvas_id"] = json!(c.id);
                                out.push(v);
                            }
                        }
                    }
                }
                Ok(json!(out))
            }
            "read_screen" | "read_scrollback" => {
                let tab = need_u64(p, "terminal_id")?;
                let (wi, ti) = self.find_term(tab).ok_or("no such terminal")?;
                let t = &self.wins[wi].terms[ti];
                let term = t.term.lock();
                let rows = term.grid().screen_lines() as i32;
                let lines = if method == "read_screen" {
                    grid_lines(&term, 0, rows - 1)
                } else {
                    let n = arg_u64(p, "lines").unwrap_or(200).clamp(1, 5000) as i32;
                    let hist = term.grid().history_size() as i32;
                    let first = (rows - n).max(-hist);
                    grid_lines(&term, first, rows - 1)
                };
                let cursor = term.grid().cursor.point;
                Ok(json!({"lines": lines, "text": lines.join("\n"), "cursor": {"line": cursor.line.0, "col": cursor.column.0}, "cols": term.grid().columns(), "rows": rows}))
            }
            "send_text" => {
                let tab = need_u64(p, "terminal_id")?;
                let text = need_str(p, "text")?.to_string();
                let enter = p.get("enter").and_then(|v| v.as_bool()).unwrap_or(false);
                let (wi, ti) = self.find_term(tab).ok_or("no such terminal")?;
                let name = self.wins[wi].terms[ti].display_title().to_string();
                if let Some(cps) = p.get("typing").and_then(|v| v.as_f64()).filter(|c| *c > 0.0) {
                    // Type it out at `typing` characters per second.
                    let mut steps: VecDeque<DripStep> = text.chars().map(|c| DripStep::Char(c, c.to_string().into_bytes())).collect();
                    if enter {
                        steps.push_back(DripStep::Char('\r', b"\r".to_vec()));
                    }
                    let tab_id = self.wins[wi].terms[ti].id;
                    self.push_drip(Drip { wi, tab: tab_id, steps, next: Instant::now(), interval: std::time::Duration::from_secs_f64((1.0 / cps).clamp(0.005, 2.0)) });
                    self.wins[wi].terms[ti].agent_touch = Some(Instant::now());
                    self.wins[wi].window.request_redraw();
                } else {
                    // Same rules as a user paste: when the program has asked
                    // for bracketed paste, wrap the text so a shell takes a
                    // multi-line block as one edit instead of running each
                    // line as it lands. Otherwise newlines become Enter.
                    let bracketed = self.wins[wi].terms[ti].term.lock().mode().contains(TermMode::BRACKETED_PASTE);
                    let mut bytes = if bracketed && text.contains('\n') {
                        let mut b = b"\x1b[200~".to_vec();
                        b.extend_from_slice(text.as_bytes());
                        b.extend_from_slice(b"\x1b[201~");
                        b
                    } else {
                        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
                    };
                    if enter {
                        bytes.push(b'\r');
                    }
                    self.wins[wi].terms[ti].write(bytes);
                    self.show_agent_input(wi, ti, &text);
                }
                let shown: String = text.chars().take(40).collect();
                self.notice(format!("typed into '{name}': {shown}{}", if text.chars().count() > 40 { "…" } else { "" }));
                Ok(json!({"ok": true}))
            }
            "send_key" => {
                let tab = need_u64(p, "terminal_id")?;
                let key = need_str(p, "key")?.to_lowercase();
                let (wi, ti) = self.find_term(tab).ok_or("no such terminal")?;
                let mode = *self.wins[wi].terms[ti].term.lock().mode();
                let app = mode.contains(TermMode::APP_CURSOR);
                let arrow = |c: char| if app { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
                let bytes: Vec<u8> = match key.as_str() {
                    "enter" | "return" => b"\r".to_vec(),
                    "tab" => b"\t".to_vec(),
                    "escape" | "esc" => b"\x1b".to_vec(),
                    "backspace" => b"\x7f".to_vec(),
                    "delete" => b"\x1b[3~".to_vec(),
                    "up" => arrow('A').into_bytes(),
                    "down" => arrow('B').into_bytes(),
                    "right" => arrow('C').into_bytes(),
                    "left" => arrow('D').into_bytes(),
                    "home" => arrow('H').into_bytes(),
                    "end" => arrow('F').into_bytes(),
                    "pageup" => b"\x1b[5~".to_vec(),
                    "pagedown" => b"\x1b[6~".to_vec(),
                    "space" => b" ".to_vec(),
                    k if k.starts_with("ctrl+") && k.len() == 6 => {
                        let c = k.as_bytes()[5].to_ascii_lowercase();
                        match c {
                            b'a'..=b'z' => vec![c & 0x1f],
                            b'[' => vec![0x1b],
                            _ => return Err(format!("unsupported key {key}")),
                        }
                    }
                    k if k.starts_with('f') && k[1..].parse::<u8>().map(|n| (1..=12).contains(&n)).unwrap_or(false) => {
                        let n: u8 = k[1..].parse().unwrap();
                        match n {
                            1 => b"\x1bOP".to_vec(),
                            2 => b"\x1bOQ".to_vec(),
                            3 => b"\x1bOR".to_vec(),
                            4 => b"\x1bOS".to_vec(),
                            5 => b"\x1b[15~".to_vec(),
                            6 => b"\x1b[17~".to_vec(),
                            7 => b"\x1b[18~".to_vec(),
                            8 => b"\x1b[19~".to_vec(),
                            9 => b"\x1b[20~".to_vec(),
                            10 => b"\x1b[21~".to_vec(),
                            11 => b"\x1b[23~".to_vec(),
                            _ => b"\x1b[24~".to_vec(),
                        }
                    }
                    _ => return Err(format!("unsupported key {key}")),
                };
                let name = self.wins[wi].terms[ti].display_title().to_string();
                let count = p.get("count").and_then(|v| v.as_u64()).unwrap_or(1).clamp(1, 2000) as usize;
                if count > 1 {
                    // Hold the key: repeats at the usual auto-repeat rate.
                    // Backspace and Delete get the Pac-Man chomp.
                    let eater = match key.as_str() { "backspace" => Some(true), "delete" => Some(false), _ => None };
                    let steps: VecDeque<DripStep> = (0..count).map(|_| match eater {
                        Some(left) => DripStep::Eat(left, bytes.clone()),
                        None => DripStep::Char('\0', bytes.clone()),
                    }).collect();
                    let tab_id = self.wins[wi].terms[ti].id;
                    self.push_drip(Drip { wi, tab: tab_id, steps, next: Instant::now(), interval: std::time::Duration::from_millis(40) });
                    self.wins[wi].terms[ti].agent_touch = Some(Instant::now());
                    self.wins[wi].window.request_redraw();
                    self.notice(format!("holding {key} ×{count} in '{name}'"));
                    return Ok(json!({"ok": true}));
                }
                self.wins[wi].terms[ti].write(bytes);
                self.show_agent_input(wi, ti, "");
                self.notice(format!("pressed {key} in '{name}'"));
                Ok(json!({"ok": true}))
            }
            "create_terminal" => {
                if let Some(cid) = arg_u64(p, "canvas_id") {
                    let (wi, ci) = self.find_canvas(cid).ok_or("no such canvas")?;
                    self.go_to(wi, ci);
                }
                let shell = self.config.shell();
                let mut launch = self.shell_launch();
                if let Some(cmd) = arg_str(p, "command") {
                    launch = Launch { program: shell.clone(), args: vec!["-c".into(), format!("{cmd}; exec {shell}")], cwd: None, title: cmd.split_whitespace().next().unwrap_or("shell").to_string(), shortcut: None };
                }
                launch.cwd = arg_str(p, "cwd").map(|s| s.to_string());
                let at = match (arg_f32(p, "x"), arg_f32(p, "y")) {
                    (Some(x), Some(y)) => {
                        let (cols, rows) = (arg_u64(p, "cols").unwrap_or(80) as usize, arg_u64(p, "rows").unwrap_or(24) as usize);
                        let (w, h) = self.win().rect_for_grid(cols.max(10), rows.max(3));
                        Some(WRect::new(x, y, w, h))
                    }
                    _ => None,
                };
                let spec = LaunchSpec { shortcut: None, cwd: launch.cwd.clone(), session: None };
                let tab = self.new_terminal_in_canvas(launch, at, Some(spec)).ok_or("could not start a terminal")?;
                if let Some(name) = arg_str(p, "name")
                    && let Some(t) = self.win_mut().term_mut(tab)
                {
                    t.custom_title = Some(name.to_string());
                }
                let item = self.win().canvas().and_then(|c| c.item_for_tab(tab)).map(|i| i.id);
                self.notice("opened a terminal".into());
                Ok(json!({"terminal_id": tab, "item_id": item, "canvas_id": self.win().canvas().map(|c| c.id)}))
            }
            "close_terminal" => {
                let tab = need_u64(p, "terminal_id")?;
                let (wi, _) = self.find_term(tab).ok_or("no such terminal")?;
                self.cur = wi;
                let ci = self.wins[wi].canvases.iter().position(|c| c.item_for_tab(tab).is_some()).ok_or("terminal has no item")?;
                self.wins[wi].active = ci;
                let name = self.wins[wi].term(tab).map(|t| t.display_title().to_string()).unwrap_or_default();
                self.close_terminal(tab, event_loop);
                self.notice(format!("closed '{name}'"));
                Ok(json!({"ok": true}))
            }
            "focus" => {
                let id = need_u64(p, "item_id")?;
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                self.go_to(wi, ci);
                self.focus_item(id);
                if let Some(t) = self.tab_of_item_pub(id) {
                    self.focus_terminal(t);
                }
                self.wins[wi].window.focus_window();
                Ok(json!({"ok": true}))
            }
            "move_item" => {
                let id = need_u64(p, "item_id")?;
                let (x, y) = (arg_f32(p, "x").ok_or("missing x")?, arg_f32(p, "y").ok_or("missing y")?);
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                let c = &mut self.wins[wi].canvases[ci];
                if c.is_single() {
                    return Err("that tab is a single maximized terminal; add a second terminal first".into());
                }
                if let Some(it) = c.item_mut(id) {
                    it.rect.x = x.round();
                    it.rect.y = y.round();
                    it.pin = None;
                }
                c.recompute_membership();
                self.wins[wi].dirty = true;
                Ok(json!({"ok": true}))
            }
            "resize_item" => {
                let id = need_u64(p, "item_id")?;
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                self.cur = wi;
                let is_image = matches!(self.wins[wi].canvases[ci].item(id).map(|i| &i.kind), Some(ItemKind::Image { .. }));
                if is_image {
                    let w = arg_f32(p, "width").ok_or("images need 'width'")?.max(48.0);
                    let c = &mut self.wins[wi].canvases[ci];
                    if let Some(it) = c.item_mut(id) {
                        let aspect = it.rect.w / (it.rect.h - TITLE_H).max(1.0);
                        it.rect.w = w.round();
                        it.rect.h = (w / aspect + TITLE_H).round();
                    }
                } else {
                    let cols = arg_u64(p, "cols").ok_or("missing cols")?.max(10) as usize;
                    let rows = arg_u64(p, "rows").ok_or("missing rows")?.max(3) as usize;
                    let (w, h) = self.wins[wi].rect_for_grid(cols, rows);
                    if self.wins[wi].canvases[ci].is_single() {
                        return Err("that tab is a single maximized terminal".into());
                    }
                    if let Some(it) = self.wins[wi].canvases[ci].item_mut(id) {
                        it.rect.w = w;
                        it.rect.h = h;
                    }
                    self.wins[wi].active = ci;
                    self.sync_mirror_sizes(id);
                    self.relayout();
                }
                self.wins[wi].canvases[ci].recompute_membership();
                self.wins[wi].dirty = true;
                Ok(json!({"ok": true}))
            }
            "rename_item" => {
                let id = need_u64(p, "item_id")?;
                let name = need_str(p, "name")?.trim().to_string();
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                let kind = self.wins[wi].canvases[ci].item(id).map(|i| i.kind.clone());
                match kind {
                    Some(ItemKind::Terminal(t)) => {
                        if let Some(term) = self.wins[wi].term_mut(t) {
                            term.custom_title = if name.is_empty() { None } else { Some(name) };
                        }
                    }
                    _ => {
                        if let Some(it) = self.wins[wi].canvases[ci].item_mut(id) {
                            it.name = if name.is_empty() { None } else { Some(name) };
                        }
                    }
                }
                self.wins[wi].dirty = true;
                self.update_window_title();
                Ok(json!({"ok": true}))
            }
            "zoom_to" => {
                if let Some(id) = arg_u64(p, "item_id") {
                    let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                    self.go_to(wi, ci);
                    if self.win().canvas().map(|c| c.is_single()).unwrap_or(true) {
                        return Ok(json!({"ok": true, "note": "single tab: already fills the window"}));
                    }
                    let l = self.win().layout.ok_or("no layout")?;
                    if let Some(c) = self.win_mut().canvas_mut()
                        && let Some(r) = c.item(id).map(|i| i.rect)
                    {
                        c.focus_prev = Some(c.target_view());
                        let mut v = c.target_view();
                        v.fit(l.area, r, 24.0);
                        c.glide_to(v, l.area);
                        c.focus = Some(id);
                    }
                } else if let Some(gid) = arg_u64(p, "group_id") {
                    let (wi, ci) = self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.group(gid).is_some()).map(|ci| (wi, ci))).ok_or("no such group")?;
                    self.go_to(wi, ci);
                    self.zoom_to_group(gid);
                } else {
                    if let Some(cid) = arg_u64(p, "canvas_id") {
                        let (wi, ci) = self.find_canvas(cid).ok_or("no such canvas")?;
                        self.go_to(wi, ci);
                    }
                    if self.on_free_canvas() {
                        self.fit_all();
                    }
                }
                self.wins[self.cur].dirty = true;
                Ok(json!({"ok": true}))
            }
            "set_viewport" => {
                let cid = need_u64(p, "canvas_id")?;
                let (wi, ci) = self.find_canvas(cid).ok_or("no such canvas")?;
                let area = self.wins[wi].layout.map(|l| l.area);
                let c = &mut self.wins[wi].canvases[ci];
                let mut v = c.target_view();
                if let Some(x) = arg_f32(p, "x") {
                    v.x = x;
                }
                if let Some(y) = arg_f32(p, "y") {
                    v.y = y;
                }
                if let Some(z) = arg_f32(p, "zoom") {
                    v.zoom = z.clamp(crate::canvas::MIN_ZOOM, crate::canvas::MAX_ZOOM);
                }
                // `animate: false` jumps (for callers doing their own stepping).
                let animate = p.get("animate").and_then(|x| x.as_bool()).unwrap_or(true);
                match (animate, area) {
                    (true, Some(area)) => c.glide_to(v, area),
                    _ => { c.settle_view(); c.view = v; }
                }
                c.focus_prev = None;
                let view = json!({"x": v.x, "y": v.y, "zoom": v.zoom});
                let area = self.wins[wi].layout.map(|l| (l.area.w, l.area.h)).unwrap_or((0.0, 0.0));
                self.wins[wi].dirty = true;
                self.wins[wi].window.request_redraw();
                Ok(json!({"ok": true, "view": view, "area_w": area.0, "area_h": area.1}))
            }
            "create_canvas" => {
                self.open_canvas_tab();
                if let Some(name) = arg_str(p, "name")
                    && let Some(c) = self.win_mut().canvas_mut()
                {
                    c.name = name.to_string();
                }
                let id = self.win().canvas().map(|c| c.id);
                Ok(json!({"canvas_id": id}))
            }
            "rename_canvas" => {
                let cid = need_u64(p, "canvas_id")?;
                let name = need_str(p, "name")?.to_string();
                let (wi, ci) = self.find_canvas(cid).ok_or("no such canvas")?;
                let single_tab = { let c = &self.wins[wi].canvases[ci]; if c.is_single() { c.tabs().next() } else { None } };
                match single_tab {
                    Some(t) => {
                        if let Some(term) = self.wins[wi].term_mut(t) {
                            term.custom_title = Some(name);
                        }
                    }
                    None => self.wins[wi].canvases[ci].name = name,
                }
                self.wins[wi].dirty = true;
                self.update_window_title();
                Ok(json!({"ok": true}))
            }
            "delete_canvas" => {
                let cid = need_u64(p, "canvas_id")?;
                let (wi, ci) = self.find_canvas(cid).ok_or("no such canvas")?;
                self.cur = wi;
                let name = self.wins[wi].tab_title(ci);
                self.close_tab(ci, event_loop);
                self.notice(format!("closed tab '{name}'"));
                Ok(json!({"ok": true}))
            }
            "create_group" => {
                let ids: Vec<ItemId> = p.get("item_ids").and_then(|v| v.as_array()).ok_or("missing item_ids")?.iter().filter_map(|v| v.as_u64()).collect();
                let first = *ids.first().ok_or("item_ids is empty")?;
                let (wi, ci) = self.find_item(first).ok_or("no such item")?;
                self.go_to(wi, ci);
                // The grouped set is the selection plus the focused item, so
                // focus one of the requested items: otherwise whatever the
                // user (or a previous tool call) left focused would join too.
                self.focus_item(first);
                if let Some(c) = self.win_mut().canvas_mut() {
                    c.selected = ids.clone();
                    c.group_sel = None;
                }
                let arrange = p.get("arrange").and_then(|v| v.as_bool()).unwrap_or(false);
                self.toggle_group(arrange);
                let gid = self.win().canvas().and_then(|c| c.group_sel);
                if let (Some(gid), Some(name)) = (gid, arg_str(p, "name"))
                    && let Some(g) = self.win_mut().canvas_mut().and_then(|c| c.group_mut(gid))
                {
                    g.name = name.to_string();
                }
                Ok(json!({"group_id": gid}))
            }
            "arrange_group" => {
                let gid: GroupId = need_u64(p, "group_id")?;
                let (wi, ci) = self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.group(gid).is_some()).map(|ci| (wi, ci))).ok_or("no such group")?;
                self.go_to(wi, ci);
                self.arrange_group(gid);
                let g = self.win().canvas().and_then(|c| c.group(gid)).map(|g| json!({"x": g.rect.x, "y": g.rect.y, "w": g.rect.w, "h": g.rect.h, "members": g.members}));
                Ok(json!({"ok": true, "group": g}))
            }
            "delete_group" => {
                let gid: GroupId = need_u64(p, "group_id")?;
                let (wi, ci) = self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.group(gid).is_some()).map(|ci| (wi, ci))).ok_or("no such group")?;
                self.go_to(wi, ci);
                self.ungroup(gid);
                Ok(json!({"ok": true}))
            }
            "rename_group" => {
                let gid: GroupId = need_u64(p, "group_id")?;
                let name = need_str(p, "name")?.trim().to_string();
                let (wi, ci) = self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.group(gid).is_some()).map(|ci| (wi, ci))).ok_or("no such group")?;
                if let Some(g) = self.wins[wi].canvases[ci].group_mut(gid) {
                    g.name = name;
                }
                self.wins[wi].dirty = true;
                self.wins[wi].window.request_redraw();
                Ok(json!({"ok": true}))
            }
            "add_to_group" => {
                // Groups are frames: grow the frame to enclose the item, and
                // membership follows from what the frame contains.
                let gid: GroupId = need_u64(p, "group_id")?;
                let id = need_u64(p, "item_id")?;
                let (wi, ci) = self.wins.iter().enumerate().find_map(|(wi, w)| w.canvases.iter().position(|c| c.group(gid).is_some()).map(|ci| (wi, ci))).ok_or("no such group")?;
                let c = &mut self.wins[wi].canvases[ci];
                let it = c.item(id).ok_or("no such item on that canvas")?;
                if it.pin.is_some() {
                    return Err("that item is pinned to the screen; unpin it first".into());
                }
                let r = it.rect;
                let pad = crate::canvas::GROUP_PAD;
                let padded = WRect::new(r.x - pad, r.y - pad, r.w + 2.0 * pad, r.h + 2.0 * pad);
                if let Some(g) = c.group_mut(gid) {
                    g.rect = g.rect.union(&padded);
                }
                c.recompute_membership();
                let members = c.group(gid).map(|g| g.members.clone()).unwrap_or_default();
                self.wins[wi].dirty = true;
                self.wins[wi].window.request_redraw();
                Ok(json!({"ok": true, "members": members}))
            }
            "pin_item" => {
                let id = need_u64(p, "item_id")?;
                let want = p.get("pinned").and_then(|v| v.as_bool()).ok_or("missing pinned")?;
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                self.go_to(wi, ci);
                if self.is_pinned(id) != want {
                    self.toggle_pin(id);
                }
                // Optional screen position for a pinned item: pixels from the
                // canvas area's top-left, clamped so it stays on screen.
                if want && let (Some(x), Some(y)) = (arg_f32(p, "x"), arg_f32(p, "y")) {
                    if let Some(it) = self.win_mut().canvas_mut().and_then(|c| c.item_mut(id)) {
                        it.rect.x = x;
                        it.rect.y = y;
                    }
                    self.settle_pin(id);
                    self.win_mut().dirty = true;
                    self.request_redraw();
                }
                Ok(json!({"ok": true}))
            }
            "mirror_terminal" => {
                let id = need_u64(p, "item_id")?;
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                self.go_to(wi, ci);
                self.mirror_item(id);
                let new_id = self.win().canvas().and_then(|c| c.focus);
                Ok(json!({"item_id": new_id}))
            }
            "set_monitor" => {
                let id = need_u64(p, "item_id")?;
                let secs = p.get("seconds").and_then(|v| v.as_u64()).map(|s| s as u32);
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                self.go_to(wi, ci);
                self.set_monitor(id, secs);
                Ok(json!({"ok": true}))
            }
            "get_activity" => {
                let mut out = Vec::new();
                for w in &self.wins {
                    for t in &w.terms {
                        out.push(json!({"terminal_id": t.id, "title": t.display_title(), "idle_seconds": t.last_output.elapsed().as_secs(), "quiet_alert": t.quiet_alert}));
                    }
                }
                Ok(json!(out))
            }
            "place_image" => {
                let path = need_str(p, "path")?.to_string();
                if let Some(cid) = arg_u64(p, "canvas_id") {
                    let (wi, ci) = self.find_canvas(cid).ok_or("no such canvas")?;
                    self.go_to(wi, ci);
                }
                self.place_image_file(std::path::PathBuf::from(path), None);
                let id = self.win().canvas().and_then(|c| c.focus);
                Ok(json!({"item_id": id}))
            }
            "remove_item" => {
                let id = need_u64(p, "item_id")?;
                let (wi, ci) = self.find_item(id).ok_or("no such item")?;
                self.go_to(wi, ci);
                let removable = self.win().canvas().and_then(|c| c.item(id)).map(|i| i.mirror || matches!(i.kind, ItemKind::Image { .. })).unwrap_or(false);
                if !removable {
                    return Err("that item is a terminal: use close_terminal".into());
                }
                self.close_item(id, event_loop);
                Ok(json!({"ok": true}))
            }
            "list_sessions" => {
                let attached: std::collections::HashSet<String> = self.wins.iter().flat_map(|w| w.terms.iter().filter_map(|t| t.session.clone())).collect();
                let list: Vec<Value> = crate::session::list_ids().into_iter().map(|id| json!({"session": id, "attached": attached.contains(&id)})).collect();
                Ok(json!(list))
            }
            "set_window" => {
                // Size the active window in pixels, or (un)maximize it. The
                // compositor answers later, so the size reported here is the
                // current one; read set_viewport's area_w/area_h after.
                let w = &self.wins[self.cur].window;
                if let Some(m) = p.get("maximized").and_then(|v| v.as_bool()) {
                    w.set_maximized(m);
                }
                if let (Some(width), Some(height)) = (arg_f32(p, "width"), arg_f32(p, "height")) {
                    w.set_maximized(false);
                    let _ = w.request_inner_size(winit::dpi::PhysicalSize::new(width.max(400.0) as u32, height.max(300.0) as u32));
                }
                let s = w.inner_size();
                Ok(json!({"width": s.width, "height": s.height, "maximized": w.is_maximized()}))
            }
            "screenshot" => {
                let path = std::path::PathBuf::from(need_str(p, "path")?);
                if path.extension().and_then(|e| e.to_str()) != Some("png") {
                    return Err("path must end in .png".into());
                }
                let l = self.win().layout.ok_or("no layout")?;
                self.wins[self.cur].batch.clear();
                self.draw_terminal(l);
                self.draw_deck(l);
                self.draw_tab_bar(l);
                self.draw_palette(l);
                self.draw_menu(l);
                let bg = with_alpha(self.theme.bg, self.config.colors.opacity.clamp(0.3, 1.0));
                let w = &mut self.wins[self.cur];
                w.renderer.screenshot(&mut w.fonts, &w.batch, bg, &path).map_err(|e| format!("{e:#}"))?;
                Ok(json!({"path": path}))
            }
            other => Err(format!("unknown method {other}")),
        }
    }
}
