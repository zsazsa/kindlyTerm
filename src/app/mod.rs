//! The application: windows, tabs, and the glue between input, the
//! terminal sessions, the Control Deck, and the renderer. Split by concern:
//!
//! - `input.rs`   keyboard and mouse handling
//! - `draw.rs`    everything that fills the per-frame batch
//! - `windows.rs` window lifecycle, tab tear-off / merge, wake-up scheduling
//! - `debug.rs`   developer hooks behind KINDLYTERM_DEBUG=1


use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event::{Event, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{TermMode, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use anyhow::Result;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

use crate::config::{ColorConfig, CommandStore, Config, DeckState, SavedCommand};
use crate::deck::{ClipField, Deck, DeckAction, DeckEnv, LaunchTarget, PageId, hotkey_matches};
use crate::effects::{EffectChange, Effects, EffectsConfig, GridSnapshot};
use crate::font::{FontSystem, GlyphKey};
use crate::theme::{Theme, builtin};
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::CursorStyle;
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
use crate::keys;
use crate::menu::{Menu, MenuAction};
use crate::palette::{Action, Item, ItemId, Mode, Palette};
use crate::deck::{ui_text, ui_text_tracked_pub, ui_text_width};
use crate::renderer::{Batch, Gpu, Renderer, Rgba, rgb, scale_rgb, with_alpha};
use crate::terminal::{GridSize, Launch, TabId, Terminal, UserEvent};

/// Borrow the read-only pieces of `App` the Deck needs, without borrowing
/// the Deck itself.
macro_rules! deck_env {
    ($self:ident, $fam:ident) => {
        DeckEnv {
            config: &$self.config,
            shortcuts: &$self.store.commands,
            state: &$self.deck_state,
            theme: &$self.theme,
            font_families: &$self.font_families,
            font_family: &$fam,
            tab_count: $self.wins[$self.cur].tabs.len(),
            gpu: &$self.gpu_name,
            scale: $self.wins[$self.cur].scale as f32,
            effects: &$self.effects,
        }
    };
}

mod debug;
mod draw;
mod input;
mod windows;

fn rgb_to_rgba(c: Rgb) -> Rgba {
    rgb([c.r, c.g, c.b])
}

fn rgba_to_rgb(c: Rgba) -> Rgb {
    Rgb { r: (c[0] * 255.0) as u8, g: (c[1] * 255.0) as u8, b: (c[2] * 255.0) as u8 }
}

fn dim(c: Rgba) -> Rgba {
    [c[0] * 0.66, c[1] * 0.66, c[2] * 0.66, c[3]]
}

/// Where a tab and its close button were drawn.
#[derive(Clone, Copy, Debug)]
struct TabHit {
    index: usize,
    x0: f32,
    x1: f32,
    close_x0: f32,
    close_x1: f32,
}

/// What the mouse is currently over, for hover highlights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    None,
    Tab(usize),
    Close(usize),
    Plus,
}

/// Pixel layout of the window contents.
#[derive(Clone, Copy, Debug)]
struct Layout {
    cell_w: f32,
    cell_h: f32,
    tab_bar_h: f32,
    pad: f32,
    /// Top-left pixel of the grid.
    grid_x: f32,
    grid_y: f32,
    cols: usize,
    rows: usize,
}

/// A tab being dragged in the tab bar.
#[derive(Clone, Copy, Debug)]
struct TabDrag {
    /// Current index of the dragged tab in this window.
    index: usize,
    /// Pointer offset from the tab's left edge at press time.
    grab_dx: f32,
    press_x: f32,
    press_y: f32,
    /// Past the movement threshold: reordering / ghost drawing active.
    active: bool,
    /// Pointer is outside the tab bar (release = tear off / drop on window).
    outside: bool,
}

/// A tab released outside its window: waiting to see whether another
/// kindlyterm window receives the pointer (merge) or not (new window).
struct PendingDrop {
    from: WindowId,
    tab: TabId,
    at: Instant,
}

/// Per-terminal presentation state that is not part of the grid: cursor
/// animation and visual effects. Lives on the `Terminal` so a terminal
/// keeps it when it moves between windows or canvases.
#[derive(Default)]
pub struct TermView {
    pub cursor_anim: CursorAnim,
    pub fx: Effects,
}

/// Animated cursor state: smooth travel between cells, a focus/click
/// pulse, and a resting "breath".
#[derive(Clone, Copy, Debug)]
pub struct CursorAnim {
    /// Where the cursor is currently drawn (pixels, top-left of the cell).
    pos: (f32, f32),
    from: (f32, f32),
    to: (f32, f32),
    move_start: Instant,
    pulse_start: Option<Instant>,
    last_input: Instant,
    /// Tab id the position belongs to; switching tabs snaps instead of sliding.
    tab: TabId,
    /// Backspace/Delete held since (start, last repeat, eats-left?).
    chomp: Option<(Instant, Instant, bool)>,
}

impl Default for CursorAnim {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorAnim {
    fn new() -> Self {
        let now = Instant::now();
        Self { pos: (0.0, 0.0), from: (0.0, 0.0), to: (0.0, 0.0), move_start: now, pulse_start: None, last_input: now, tab: 0, chomp: None }
    }
    const CHOMP_AFTER_MS: u128 = 1400;
    /// Pac-Man mode: held long enough and still being repeated recently.
    fn chomping(&self) -> Option<bool> {
        let (start, last, left) = self.chomp?;
        (start.elapsed().as_millis() >= Self::CHOMP_AFTER_MS && last.elapsed().as_millis() < 350).then_some(left)
    }
    const TRAVEL_MS: f32 = 90.0;
    const PULSE_MS: f32 = 480.0;
    fn travel_t(&self) -> f32 {
        (self.move_start.elapsed().as_secs_f32() * 1000.0 / Self::TRAVEL_MS).min(1.0)
    }
    fn pulse_t(&self) -> Option<f32> {
        let t = self.pulse_start?.elapsed().as_secs_f32() * 1000.0 / Self::PULSE_MS;
        (t < 1.0).then_some(t)
    }
    /// True while a short (60 fps) animation is running.
    fn transient(&self) -> bool {
        self.travel_t() < 1.0 || self.pulse_t().is_some() || self.chomp.is_some()
    }
}

/// One top-level window with its own GPU surface, font atlas, and tabs.
struct Win {
    window: Arc<Window>,
    renderer: Renderer,
    fonts: FontSystem,
    batch: Batch,
    layout: Option<Layout>,
    scale: f64,

    tabs: Vec<Terminal>,
    active: usize,

    palette: Option<Palette>,
    menu: Option<Menu>,
    /// Pixel extents of each tab drawn last frame, for mouse hit-testing.
    tab_hits: Vec<TabHit>,
    /// Pixel extents of the "+" button drawn last frame.
    plus_hit: Option<(f32, f32)>,
    hover: Hover,
    mouse: PhysicalPosition<f64>,
    selecting: bool,
    last_click: Option<(Instant, Point)>,
    click_count: u8,
    focused: bool,
    /// Message shown briefly in the tab bar (e.g. "saved 'homelab'").
    status: Option<(String, Instant)>,
    frame: u64,
    deck: Deck,
    drag: Option<TabDrag>,
    /// Inline tab rename in progress: (tab index, text so far, whole title
    /// selected so the next keystroke replaces it).
    rename: Option<(usize, String, bool)>,
    last_tab_click: Option<(Instant, usize)>,
    /// Keyboard cheat sheet overlay (Ctrl+/).
    cheat: bool,
}

impl Win {
    /// Presentation state of the active terminal, if any.
    fn view_mut(&mut self) -> Option<&mut TermView> {
        let i = self.active;
        self.tabs.get_mut(i).map(|t| &mut t.view)
    }
    fn view(&self) -> Option<&TermView> {
        self.tabs.get(self.active).map(|t| &t.view)
    }
    /// Any terminal in this window still animating?
    fn any_view_transient(&self) -> bool {
        self.tabs.iter().any(|t| t.view.cursor_anim.transient() || t.view.fx.active())
    }
}

pub struct App {
    config: Config,
    theme: Theme,
    store: CommandStore,
    proxy: EventLoopProxy<UserEvent>,

    /// All open windows; `cur` is the one the event being handled belongs to.
    wins: Vec<Win>,
    cur: usize,
    font_pt: f32,
    next_id: TabId,
    mods: ModifiersState,
    clipboard: Option<arboard::Clipboard>,
    debug: DebugOptions,
    pending_drop: Option<PendingDrop>,
    gpu: Option<Arc<Gpu>>,
    /// Next time an animation frame is due (cursor breath/travel/pulse).
    next_frame: Option<Instant>,
    effects: EffectsConfig,

    deck_state: DeckState,
    /// Set when Ctrl+Shift are both held with nothing else; cleared by any
    /// other key or click. Releasing while set toggles the Deck.
    chord_armed: Option<Instant>,
    font_families: Vec<String>,
    gpu_name: String,
    /// Colors/font in effect before a live preview started, to revert to.
    preview_colors: Option<ColorConfig>,
    preview_font: Option<String>,
}

/// Developer knobs read from the environment.
#[derive(Default)]
struct DebugOptions {
    /// KINDLYTERM_SCREENSHOT=path.png : save an offscreen render of a frame.
    screenshot: Option<std::path::PathBuf>,
    /// KINDLYTERM_SCREENSHOT_FRAME=n : which frame to capture (default 30).
    screenshot_frame: u64,
    /// KINDLYTERM_ACTIONS_FRAME=n : frame at which KINDLYTERM_KEYS actions run
    /// (default: the frame before the screenshot).
    actions_frame: u64,
    /// KINDLYTERM_INPUT="ls\r" : bytes typed into the first tab at startup.
    input: Option<String>,
    /// KINDLYTERM_KEYS="palette,scroll:40,tabs" : actions applied just before
    /// the screenshot frame.
    actions: Vec<String>,
    /// KINDLYTERM_EXIT_AFTER=ms : quit after this many milliseconds. If a
    /// screenshot is configured but not yet taken, it is taken right before.
    exit_after: Option<u64>,
    /// KINDLYTERM_ACTIONS_AFTER=ms : run the KINDLYTERM_KEYS actions after a delay
    /// instead of at a frame number.
    actions_after: Option<u64>,
    shot_done: bool,
    /// Running index for `frames:` captures, so bursts do not overwrite.
    frame_seq: u32,
    /// `termzoom:Z` draws the active terminal at zoom Z inside a clipped
    /// box, to exercise the canvas drawing path before the canvas exists.
    term_zoom: Option<f32>,
}

impl DebugOptions {
    fn from_env() -> Self {
        // Every developer hook is inert unless KINDLYTERM_DEBUG=1 is set, so a
        // stray environment variable can never type into a shell or write
        // files on a user's behalf.
        if std::env::var("KINDLYTERM_DEBUG").ok().as_deref() != Some("1") {
            return Self { screenshot_frame: 30, ..Self::default() };
        }
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            screenshot: get("KINDLYTERM_SCREENSHOT").map(Into::into),
            screenshot_frame: get("KINDLYTERM_SCREENSHOT_FRAME").and_then(|v| v.parse().ok()).unwrap_or(30),
            actions_frame: get("KINDLYTERM_ACTIONS_FRAME").and_then(|v| v.parse().ok()).unwrap_or(0),
            input: get("KINDLYTERM_INPUT").map(|s| s.replace("\\r", "\r").replace("\\n", "\n").replace("\\t", "\t").replace("\\e", "\x1b")),
            actions: get("KINDLYTERM_KEYS").map(|v| v.split(',').map(str::to_string).collect()).unwrap_or_default(),
            exit_after: get("KINDLYTERM_EXIT_AFTER").and_then(|v| v.parse().ok()),
            actions_after: get("KINDLYTERM_ACTIONS_AFTER").and_then(|v| v.parse().ok()),
            shot_done: false,
            frame_seq: 0,
            term_zoom: None,
        }
    }
}
impl App {
    pub fn new(config: Config, store: CommandStore, proxy: EventLoopProxy<UserEvent>) -> Self {
        let theme = Theme::from_config(&config.colors);
        let font_pt = config.font.size;
        Self {
            config,
            theme,
            store,
            proxy,
            wins: Vec::new(),
            cur: 0,
            font_pt,
            next_id: 1,
            mods: ModifiersState::empty(),
            clipboard: arboard::Clipboard::new().ok(),
            debug: DebugOptions::from_env(),
            pending_drop: None,
            gpu: None,
            next_frame: None,
            effects: EffectsConfig::load(),
            deck_state: DeckState::load(),
            chord_armed: None,
            font_families: Vec::new(),
            gpu_name: String::new(),
            preview_colors: None,
            preview_font: None,
        }
    }

    // -----------------------------------------------------------------------
    // Layout & sizing
    // -----------------------------------------------------------------------

    fn compute_layout(config: &Config, w: &Win) -> Layout {
        let m = w.fonts.metrics;
        let s = w.scale as f32;
        let pad = (config.terminal.padding * s).round();
        // Tab bar: 40 px at scale 1 (design), never shorter than 1.6 cells.
        let tab_bar_h = (40.0 * s).max(m.height * 1.6).round();
        let width = w.renderer.width as f32;
        let height = w.renderer.height as f32;
        let cols = (((width - 2.0 * pad) / m.width).floor() as usize).max(2);
        let rows = (((height - tab_bar_h - 2.0 * pad) / m.height).floor() as usize).max(1);
        Layout {
            cell_w: m.width,
            cell_h: m.height,
            tab_bar_h,
            pad,
            grid_x: pad,
            grid_y: tab_bar_h + pad,
            cols,
            rows,
        }
    }

    fn grid_size_of(w: &Win) -> GridSize {
        let l = w.layout.expect("layout");
        GridSize { cols: l.cols, rows: l.rows, cell_width: l.cell_w as u16, cell_height: l.cell_h as u16 }
    }

    fn grid_size(&self) -> GridSize {
        Self::grid_size_of(self.win())
    }

    /// Recompute a window's layout and resize its tabs to match.
    fn relayout_win(&mut self, wi: usize) {
        let l = Self::compute_layout(&self.config, &self.wins[wi]);
        let w = &mut self.wins[wi];
        w.layout = Some(l);
        let size = Self::grid_size_of(w);
        for tab in &mut w.tabs {
            tab.resize(size);
        }
        w.window.request_redraw();
    }

    fn relayout(&mut self) {
        let i = self.cur;
        self.relayout_win(i);
    }

    fn relayout_all(&mut self) {
        for i in 0..self.wins.len() {
            self.relayout_win(i);
        }
    }

    fn set_font_pt(&mut self, pt: f32) {
        let pt = pt.clamp(6.0, 72.0);
        if (pt - self.font_pt).abs() < 0.01 {
            return;
        }
        self.font_pt = pt;
        let lp = self.config.font.line_padding;
        for w in &mut self.wins {
            w.fonts.set_size(pt * w.scale as f32, lp * w.scale as f32);
        }
        self.relayout_all();
    }

    fn request_redraw(&self) {
        if let Some(w) = self.wins.get(self.cur) {
            w.window.request_redraw();
        }
    }

    fn request_redraw_all(&self) {
        for w in &self.wins {
            w.window.request_redraw();
        }
    }

    fn win(&self) -> &Win {
        &self.wins[self.cur]
    }

    fn win_mut(&mut self) -> &mut Win {
        let i = self.cur;
        &mut self.wins[i]
    }

    // -----------------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------------

    fn shell_launch(&self) -> Launch {
        let shell = self.config.shell();
        let title = std::path::Path::new(&shell)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| shell.clone());
        Launch { program: shell, args: self.config.terminal.shell_args.clone(), cwd: None, title }
    }

    fn command_launch(&self, cmd: &SavedCommand) -> Launch {
        let shell = self.config.shell();
        let line = if cmd.keep_open {
            format!("{}; exec {}", cmd.command, shell)
        } else {
            cmd.command.clone()
        };
        Launch {
            program: shell,
            args: vec!["-lc".into(), line],
            cwd: cmd.cwd.clone().map(|c| shellexpand_home(&c)),
            title: cmd.name.clone(),
        }
    }

    /// alacritty options derived from the config.
    fn term_config(&self) -> TermConfig {
        let shape = match self.config.terminal.cursor.to_lowercase().as_str() {
            "beam" => CursorShape::Beam,
            "underline" => CursorShape::Underline,
            _ => CursorShape::Block,
        };
        TermConfig {
            scrolling_history: self.config.terminal.scrollback,
            default_cursor_style: CursorStyle { shape, blinking: false },
            ..TermConfig::default()
        }
    }

    fn open_tab(&mut self, launch: Launch) {
        if self.wins.get(self.cur).map(|w| w.layout.is_none()).unwrap_or(true) {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        match Terminal::spawn(id, self.proxy.clone(), &launch, self.grid_size(), self.term_config()) {
            Ok(term) => {
                let w = self.win_mut();
                w.tabs.push(term);
                w.active = w.tabs.len() - 1;
                self.update_window_title();
                self.request_redraw();
            }
            Err(e) => {
                log::error!("failed to open tab: {e:#}");
                self.set_status(format!("failed: {e}"));
            }
        }
    }

    fn close_tab(&mut self, index: usize, event_loop: &ActiveEventLoop) {
        if index >= self.wins[self.cur].tabs.len() {
            return;
        }
        let term = self.wins[self.cur].tabs.remove(index);
        term.shutdown();
        drop(term);
        if self.wins[self.cur].tabs.is_empty() {
            self.close_window(self.cur, event_loop);
            return;
        }
        let w = self.win_mut();
        if w.active >= w.tabs.len() {
            w.active = w.tabs.len() - 1;
        } else if index < w.active {
            w.active -= 1;
        }
        self.update_window_title();
        self.request_redraw();
    }

    fn close_others(&mut self, keep: usize, event_loop: &ActiveEventLoop) {
        if keep >= self.wins[self.cur].tabs.len() {
            return;
        }
        let w = self.win_mut();
        let kept = w.tabs.remove(keep);
        for t in w.tabs.drain(..) {
            t.shutdown();
        }
        w.tabs.push(kept);
        w.active = 0;
        self.update_window_title();
        self.request_redraw();
        let _ = event_loop;
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.wins[self.cur].tabs.len() || to >= self.wins[self.cur].tabs.len() || from == to {
            return;
        }
        let w = self.win_mut();
        let t = w.tabs.remove(from);
        w.tabs.insert(to, t);
        if w.active == from {
            w.active = to;
        } else if from < w.active && to >= w.active {
            w.active -= 1;
        } else if from > w.active && to <= w.active {
            w.active += 1;
        }
        self.request_redraw();
    }

    fn switch_tab(&mut self, index: usize) {
        if index < self.wins[self.cur].tabs.len() && index != self.wins[self.cur].active {
            self.wins[self.cur].active = index;
            self.wins[self.cur].selecting = false;
            self.update_window_title();
            self.request_redraw();
        }
    }

    fn start_rename(&mut self, index: usize) {
        let Some(current) = self.win().tabs.get(index).map(|t| t.display_title().to_string()) else { return };
        let w = self.win_mut();
        w.drag = None;
        w.rename = Some((index, current, true));
        self.request_redraw();
    }

    /// Finish an inline rename. `commit` false discards the edit.
    fn end_rename(&mut self, commit: bool) {
        let Some((index, text, _)) = self.win_mut().rename.take() else { return };
        if commit {
            let text = text.trim().to_string();
            if let Some(t) = self.win_mut().tabs.get_mut(index) {
                t.custom_title = if text.is_empty() { None } else { Some(text) };
            }
            self.update_window_title();
        }
        self.request_redraw();
    }

    fn rename_key(&mut self, event: &KeyEvent) {
        let ctrl = self.mods.control_key();
        // Helper: apply an edit to the rename text, honouring select-all.
        fn edit(w: &mut Win, f: impl FnOnce(&mut String, bool)) {
            if let Some((_, text, all)) = w.rename.as_mut() {
                let was_all = *all;
                *all = false;
                f(text, was_all);
            }
        }
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => self.end_rename(true),
            Key::Named(NamedKey::Escape) => self.end_rename(false),
            // Right/End/Left: keep the title, put the caret at the end to append.
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::End) | Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::Home) => {
                edit(self.win_mut(), |_, _| {});
                self.request_redraw();
            }
            Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete) => {
                edit(self.win_mut(), |text, all| {
                    if all {
                        text.clear();
                    } else if ctrl {
                        let cut = text.trim_end().rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                        text.truncate(cut);
                    } else {
                        text.pop();
                    }
                });
                self.request_redraw();
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("u") => {
                edit(self.win_mut(), |text, _| text.clear());
                self.request_redraw();
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                if let Some((_, _, all)) = self.win_mut().rename.as_mut() {
                    *all = true;
                }
                self.request_redraw();
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                let paste = self.clipboard.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default();
                let line = paste.lines().next().unwrap_or("").to_string();
                edit(self.win_mut(), |text, all| {
                    if all {
                        text.clear();
                    }
                    text.push_str(&line);
                });
                self.request_redraw();
            }
            Key::Named(NamedKey::Space) => {
                edit(self.win_mut(), |text, all| {
                    if all {
                        text.clear();
                    }
                    text.push(' ');
                });
                self.request_redraw();
            }
            Key::Character(_) if !ctrl && !self.mods.alt_key() => {
                if let Some(t) = event.text.as_deref().map(str::to_string) {
                    edit(self.win_mut(), |text, all| {
                        if all {
                            text.clear();
                        }
                        if text.chars().count() < 60 {
                            text.push_str(&t);
                        }
                    });
                }
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn active_tab(&self) -> Option<&Terminal> {
        self.wins[self.cur].tabs.get(self.wins[self.cur].active)
    }

    fn update_window_title(&self) {
        if let Some(w) = self.wins.get(self.cur)
            && let Some(t) = w.tabs.get(w.active) {
                w.window.set_title(&format!("{} — kindlyterm", t.display_title()));
            }
    }

    fn set_status(&mut self, msg: String) {
        log::info!("{msg}");
        self.wins[self.cur].status = Some((msg, Instant::now()));
        self.request_redraw();
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    fn open_palette(&mut self, mode: Mode) {
        // The Deck replaced the old command palette and save flow.
        match &mode {
            Mode::Commands | Mode::ConfirmDelete { .. } => {
                self.wins[self.cur].deck.open(PageId::Home);
                self.request_redraw();
                return;
            }
            Mode::SaveCommand { .. } => {
                self.open_shortcut_editor();
                return;
            }
            Mode::Tabs => {}
        }
        let items = match mode {
            Mode::Commands | Mode::ConfirmDelete { .. } => self
                .store
                .commands
                .iter()
                .map(|c| Item { id: ItemId::Command(c.name.clone()), label: c.name.clone(), detail: c.command.clone() })
                .collect(),
            Mode::Tabs => self
                .win()
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| Item {
                    id: ItemId::Tab(t.id),
                    label: format!("{}: {}", i + 1, t.display_title()),
                    detail: t.base_title.clone(),
                })
                .collect(),
            Mode::SaveCommand { .. } => Vec::new(),
        };
        let mut p = Palette::new(mode, items);
        if p.mode == Mode::Tabs {
            p.selected = self.wins[self.cur].active;
        }
        self.wins[self.cur].palette = Some(p);
        self.request_redraw();
    }

    fn palette_key(&mut self, event: &KeyEvent) {
        let mods = self.mods;
        let ctrl = mods.control_key();
        let Some(p) = self.wins[self.cur].palette.as_mut() else { return };

        if let Mode::ConfirmDelete { .. } = p.mode {
            let action = match &event.logical_key {
                Key::Character(c) if c.eq_ignore_ascii_case("y") => p.confirm_delete(true),
                Key::Named(NamedKey::Escape) => p.confirm_delete(false),
                Key::Character(_) | Key::Named(NamedKey::Enter) => p.confirm_delete(false),
                _ => Action::None,
            };
            self.apply_palette_action(action);
            return;
        }

        let action = match &event.logical_key {
            Key::Named(NamedKey::Escape) => Action::Close,
            Key::Named(NamedKey::Enter) => p.confirm(),
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) if !mods.shift_key() => {
                p.move_selection(1);
                Action::None
            }
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::Tab) => {
                p.move_selection(-1);
                Action::None
            }
            Key::Named(NamedKey::PageDown) => {
                p.move_selection(10);
                Action::None
            }
            Key::Named(NamedKey::PageUp) => {
                p.move_selection(-10);
                Action::None
            }
            Key::Named(NamedKey::Backspace) => {
                p.backspace(ctrl);
                Action::None
            }
            Key::Named(NamedKey::Space) => {
                p.insert_str(" ");
                Action::None
            }
            Key::Character(c) if ctrl => {
                match c.to_ascii_lowercase().as_str() {
                    "n" | "j" => p.move_selection(1),
                    "p" | "k" => p.move_selection(-1),
                    "u" => p.clear_input(),
                    "w" => p.backspace(true),
                    "d" => p.request_delete(),
                    "v" => {
                        if let Some(text) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) {
                            p.insert_str(text.trim());
                        }
                    }
                    _ => {}
                }
                Action::None
            }
            Key::Character(_) => {
                if let Some(text) = event.text.as_deref() {
                    p.insert_str(text);
                }
                Action::None
            }
            _ => Action::None,
        };
        self.apply_palette_action(action);
    }

    fn apply_palette_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Close => self.wins[self.cur].palette = None,
            Action::LaunchCommand(name) => {
                self.wins[self.cur].palette = None;
                if let Some(cmd) = self.store.commands.iter().find(|c| c.name == name).cloned() {
                    let launch = self.command_launch(&cmd);
                    self.open_tab(launch);
                }
            }
            Action::SwitchTab(id) => {
                self.wins[self.cur].palette = None;
                if let Some(i) = self.wins[self.cur].tabs.iter().position(|t| t.id == id) {
                    self.switch_tab(i);
                }
            }
            Action::SaveCommand(cmd) => {
                self.wins[self.cur].palette = None;
                let name = cmd.name.clone();
                self.store.upsert(cmd);
                match self.store.save() {
                    Ok(()) => self.set_status(format!("saved '{name}'")),
                    Err(e) => self.set_status(format!("save failed: {e}")),
                }
            }
            Action::DeleteCommand(name) => {
                self.store.remove(&name);
                if let Err(e) = self.store.save() {
                    self.set_status(format!("save failed: {e}"));
                } else {
                    self.set_status(format!("deleted '{name}'"));
                }
                // Rebuild the list.
                self.open_palette(Mode::Commands);
            }
        }
        self.request_redraw();
    }

    // -----------------------------------------------------------------------
    // Control Deck
    // -----------------------------------------------------------------------

    /// Open the shortcut editor, prefilled with the selection if any.
    fn open_shortcut_editor(&mut self) {
        let prefill = self
            .active_tab()
            .and_then(|t| t.term.lock().selection_to_string())
            .filter(|s| !s.trim().is_empty() && !s.contains('\n'))
            .map(|s| s.trim().to_string());
        self.wins[self.cur].deck.edit_shortcut(None, prefill);
        self.request_redraw();
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.set_status(format!("config save failed: {e}"));
        }
    }

    /// Load `family` into every window. Returns false (and leaves the old
    /// font) if it cannot be loaded.
    fn reload_font(&mut self, family: &str) -> bool {
        let lp = self.config.font.line_padding;
        let mut loaded = Vec::with_capacity(self.wins.len());
        for w in &self.wins {
            match FontSystem::new(family, self.font_pt * w.scale as f32, lp * w.scale as f32) {
                Ok(f) => loaded.push(f),
                Err(e) => {
                    self.set_status(format!("font failed: {e}"));
                    return false;
                }
            }
        }
        for (w, f) in self.wins.iter_mut().zip(loaded) {
            w.fonts = f;
        }
        self.relayout_all();
        true
    }

    fn apply_term_options(&self) {
        let cfg = self.term_config();
        for w in &self.wins {
            for t in &w.tabs {
                t.set_options(cfg.clone());
            }
        }
    }

    fn launch_shortcut(&mut self, name: &str, target: LaunchTarget) {
        let Some(cmd) = self.store.commands.iter().find(|c| c.name == name).cloned() else {
            self.set_status(format!("no shortcut '{name}'"));
            return;
        };
        let here = match target {
            LaunchTarget::Saved => cmd.runs_here(),
            LaunchTarget::Here => true,
            LaunchTarget::NewTab => false,
        };
        self.deck_state.record_run(&cmd.name);
        if here {
            if let Some(tab) = self.wins[self.cur].tabs.get(self.wins[self.cur].active) {
                tab.scroll(Scroll::Bottom);
                tab.write(format!("{}\r", cmd.command).into_bytes());
            }
            self.wins[self.cur].deck.set_toast(format!("{} → this tab", cmd.name));
        } else {
            let launch = self.command_launch(&cmd);
            self.open_tab(launch);
            self.wins[self.cur].deck.set_toast(format!("{} → new tab", cmd.name));
        }
        self.request_redraw();
    }

    fn apply_deck_action(&mut self, action: DeckAction) {
        match action {
            DeckAction::None => {}
            DeckAction::Launch { name, target } => {
                let confirm = self.store.commands.iter().find(|c| c.name == name).map(|c| c.confirm).unwrap_or(false);
                if confirm {
                    let (w, h) = Some(&self.wins[self.cur].renderer).map(|r| (r.width as f32, r.height as f32)).unwrap_or((800.0, 600.0));
                    self.wins[self.cur].menu = Some(Menu::confirm_run(w / 2.0 - 120.0, h / 2.0 - 40.0, &name, target));
                } else {
                    self.launch_shortcut(&name, target);
                }
            }
            DeckAction::RunRaw(text) => {
                let cmd = SavedCommand::new(text.clone(), text);
                let launch = self.command_launch(&cmd);
                self.open_tab(launch);
            }
            DeckAction::PreviewTheme(Some(name)) => {
                if let Some(t) = builtin(&name) {
                    if self.preview_colors.is_none() {
                        self.preview_colors = Some(self.config.colors.clone());
                    }
                    self.theme = Theme::from_config(&t.to_config());
                }
            }
            DeckAction::PreviewTheme(None) => {
                if let Some(c) = self.preview_colors.take() {
                    self.theme = Theme::from_config(&c);
                }
            }
            DeckAction::ApplyTheme(name) => {
                if let Some(t) = builtin(&name) {
                    self.config.colors = t.to_config();
                    self.theme = Theme::from_config(&self.config.colors);
                    self.preview_colors = None;
                    self.save_config();
                }
            }
            DeckAction::PreviewFont(Some(family)) => {
                if self.preview_font.is_none() {
                    self.preview_font = Some(self.config.font.family.clone());
                }
                if Some(&self.wins[self.cur].fonts).map(|f| !f.family.eq_ignore_ascii_case(&family)).unwrap_or(true) {
                    self.reload_font(&family);
                }
            }
            DeckAction::PreviewFont(None) => {
                if let Some(fam) = self.preview_font.take()
                    && Some(&self.wins[self.cur].fonts).map(|f| !f.family.eq_ignore_ascii_case(&fam)).unwrap_or(true) {
                        self.reload_font(&fam);
                    }
            }
            DeckAction::ApplyFont(family) => {
                if self.reload_font(&family) {
                    self.config.font.family = family;
                    self.preview_font = None;
                    self.save_config();
                }
            }
            DeckAction::SetFontSize(pt) => {
                let pt = pt.clamp(6.0, 72.0);
                self.config.font.size = pt;
                self.set_font_pt(pt);
                self.save_config();
            }
            DeckAction::SetLinePadding(px) => {
                self.config.font.line_padding = px.clamp(0.0, 20.0);
                let (pt, lp) = (self.font_pt, self.config.font.line_padding);
                for w in &mut self.wins {
                    w.fonts.set_size(pt * w.scale as f32, lp * w.scale as f32);
                }
                self.relayout_all();
                self.save_config();
            }
            DeckAction::Effect(change) => {
                self.effects.apply(change);
                if change != EffectChange::Reload
                    && let Err(e) = self.effects.save() {
                        self.set_status(format!("effects save failed: {e}"));
                    }
                let name = self.effects.preset.clone();
                self.win_mut().deck.set_toast(format!("effects: {name}"));
            }
            DeckAction::SetOpacity(o) => {
                self.config.colors.opacity = (o * 100.0).round() / 100.0;
                self.save_config();
            }
            DeckAction::SetPadding(px) => {
                self.config.terminal.padding = px.clamp(0.0, 64.0);
                self.relayout_all();
                self.save_config();
            }
            DeckAction::SetCursor(shape) => {
                self.config.terminal.cursor = shape;
                self.apply_term_options();
                self.save_config();
            }
            DeckAction::SetScrollback(n) => {
                self.config.terminal.scrollback = n.clamp(0, 200_000);
                self.apply_term_options();
                self.save_config();
            }
            DeckAction::SetShell(shell) => {
                self.config.terminal.shell = shell;
                self.save_config();
            }
            DeckAction::SetClipboard(field, on) => {
                let c = &mut self.config.clipboard;
                match field {
                    ClipField::CopyOnSelect => c.copy_on_select = on,
                    ClipField::CtrlC => c.ctrl_c_copies_selection = on,
                    ClipField::CtrlV => c.ctrl_v_pastes_in_shell = on,
                    ClipField::TrimNewline => c.trim_trailing_newline = on,
                }
                self.save_config();
            }
            DeckAction::SaveShortcut { cmd, previous } => {
                if let Some(prev) = previous.filter(|p| *p != cmd.name) {
                    self.store.remove(&prev);
                }
                self.store.upsert(cmd);
                if let Err(e) = self.store.save() {
                    self.set_status(format!("save failed: {e}"));
                }
            }
            DeckAction::DeleteShortcut(name) => {
                self.store.remove(&name);
                if let Err(e) = self.store.save() {
                    self.set_status(format!("save failed: {e}"));
                }
            }
            DeckAction::ReloadCommands => {
                self.store = CommandStore::load();
                let n = self.store.commands.len();
                self.win_mut().deck.set_toast(format!("loaded {n} shortcuts"));
            }
        }
        self.request_redraw_all();
    }

    // -----------------------------------------------------------------------
    // Context menu
    // -----------------------------------------------------------------------

    fn open_tab_bar_menu(&mut self, x: f32, y: f32) {
        let tab = self.tab_at(x).map(|h| h.index);
        let me = self.win().window.id();
        let others: Vec<(WindowId, String)> = self
            .wins
            .iter()
            .filter(|w| w.window.id() != me)
            .map(|w| {
                let title = w.tabs.get(w.active).map(|t| t.display_title().to_string()).unwrap_or_default();
                let title = if w.tabs.len() > 1 { format!("{title} (+{})", w.tabs.len() - 1) } else { title };
                (w.window.id(), title)
            })
            .collect();
        let n = self.win().tabs.len();
        let has_saved = !self.store.commands.is_empty();
        self.win_mut().menu = Some(Menu::for_tab_bar(x, y, tab, n, has_saved, &others));
        self.request_redraw();
    }

    fn open_terminal_menu(&mut self, x: f32, y: f32) {
        let has_selection = self
            .active_tab()
            .map(|t| t.term.lock().selection.as_ref().map(|s| !s.is_empty()).unwrap_or(false))
            .unwrap_or(false);
        self.wins[self.cur].menu = Some(Menu::for_terminal(x, y, self.wins[self.cur].active, has_selection, !self.store.commands.is_empty()));
        self.request_redraw();
    }

    fn run_menu_action(&mut self, action: MenuAction, event_loop: &ActiveEventLoop) {
        self.wins[self.cur].menu = None;
        match action {
            MenuAction::NewTab => {
                let l = self.shell_launch();
                self.open_tab(l);
            }
            MenuAction::RunSaved => {
                self.wins[self.cur].deck.open(PageId::Home);
            }
            MenuAction::SaveCommand => self.open_shortcut_editor(),
            MenuAction::RunShortcut { ref name, target } => {
                let name = name.clone();
                self.launch_shortcut(&name, target);
            }
            MenuAction::OpenDeck => {
                let a = self.wins[self.cur].deck.toggle();
                self.apply_deck_action(a);
            }
            MenuAction::CloseTab(i) => self.close_tab(i, event_loop),
            MenuAction::CloseOthers(i) => self.close_others(i, event_loop),
            MenuAction::MoveLeft(i) => self.move_tab(i, i.saturating_sub(1)),
            MenuAction::MoveRight(i) => self.move_tab(i, i + 1),
            MenuAction::Copy => {
                self.copy_selection();
            }
            MenuAction::Paste => self.paste(),
            MenuAction::ClearScrollback => {
                if let Some(tab) = self.wins[self.cur].tabs.get(self.wins[self.cur].active) {
                    let mut term = tab.term.lock();
                    term.grid_mut().clear_history();
                    term.scroll_display(Scroll::Bottom);
                }
            }
            MenuAction::NewWindow => {
                self.create_window(event_loop, Vec::new());
            }
            MenuAction::CheatSheet => {
                self.win_mut().cheat = true;
            }
            MenuAction::PasteConfirmed(text) => {
                // Bypass the confirmation this time; still arm the paste rain.
                self.arm_paste_rain(&text);
                let w = self.win();
                let tab = &w.tabs[w.active];
                let raw = text.replace("\r\n", "\r").replace('\n', "\r");
                tab.write(raw.into_bytes());
                tab.scroll(Scroll::Bottom);
            }
            MenuAction::RenameTab(i) => {
                self.switch_tab(i);
                self.start_rename(i);
            }
            MenuAction::TearOff(i) => {
                if let Some(tab) = self.win().tabs.get(i).map(|t| t.id) {
                    let from = self.cur;
                    self.tear_off(from, tab, event_loop);
                }
            }
            MenuAction::MoveToWindow(i, id) => {
                if let (Some(tab), Some(to)) = (self.win().tabs.get(i).map(|t| t.id), self.window_index(id)) {
                    let from = self.cur;
                    self.move_tab_to_window(from, tab, to, event_loop);
                }
            }
            MenuAction::Separator | MenuAction::Cancel => {}
        }
        self.request_redraw();
    }

    fn menu_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        let Some(menu) = self.wins[self.cur].menu.as_mut() else { return };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.wins[self.cur].menu = None,
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => menu.move_selection(1),
            Key::Named(NamedKey::ArrowUp) => menu.move_selection(-1),
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                if let Some(a) = menu.selected_action() {
                    self.run_menu_action(a, event_loop);
                }
            }
            Key::Character(c) if self.mods.control_key() => match c.to_ascii_lowercase().as_str() {
                "n" | "j" => menu.move_selection(1),
                "p" | "k" => menu.move_selection(-1),
                _ => {}
            },
            _ => {}
        }
        self.request_redraw();
    }

    // -----------------------------------------------------------------------
    // Terminal events
    // -----------------------------------------------------------------------

    fn on_term_event(&mut self, tab_id: TabId, event: Event, event_loop: &ActiveEventLoop) {
        let Some(index) = self.wins[self.cur].tabs.iter().position(|t| t.id == tab_id) else { return };
        match event {
            Event::Wakeup => self.request_redraw(),
            Event::Title(title) => {
                self.wins[self.cur].tabs[index].title = Some(title);
                self.update_window_title();
                self.request_redraw();
            }
            Event::ResetTitle => {
                self.wins[self.cur].tabs[index].title = None;
                self.update_window_title();
                self.request_redraw();
            }
            Event::ClipboardStore(_, text) => {
                // OSC 52 write: allowed unless the policy is "none".
                let policy = self.config.terminal.osc52.to_lowercase();
                if policy == "copy" || policy == "both" {
                    if let Some(cb) = self.clipboard.as_mut() {
                        let _ = cb.set_text(text);
                    }
                    self.set_status("a program copied to the clipboard".into());
                }
            }
            Event::ClipboardLoad(_, format) => {
                // OSC 52 read: only when explicitly enabled ("both"). Otherwise
                // answer with an empty selection so the program does not hang.
                let allowed = self.config.terminal.osc52.eq_ignore_ascii_case("both");
                let text = if allowed { self.clipboard.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default() } else { String::new() };
                if !allowed {
                    log::info!("blocked an OSC 52 clipboard read (set terminal.osc52 = \"both\" to allow)");
                }
                self.wins[self.cur].tabs[index].write(format(&text).into_bytes());
            }
            Event::ColorRequest(idx, format) => {
                let color = self.color_for_index(idx);
                self.wins[self.cur].tabs[index].write(format(rgba_to_rgb(color)).into_bytes());
            }
            Event::PtyWrite(text) => self.wins[self.cur].tabs[index].write(text.into_bytes()),
            Event::TextAreaSizeRequest(format) => {
                let size: WindowSize = self.wins[self.cur].tabs[index].size.into();
                self.wins[self.cur].tabs[index].write(format(size).into_bytes());
            }
            Event::Exit => self.close_tab(index, event_loop),
            Event::ChildExit(_) => {
                self.wins[self.cur].tabs[index].exited = true;
            }
            Event::Bell | Event::CursorBlinkingChange | Event::MouseCursorDirty => {}
        }
    }

    fn color_for_index(&self, idx: usize) -> Rgba {
        match idx {
            0..=15 => self.theme.ansi[idx],
            256 => self.theme.fg,
            257 => self.theme.bg,
            258 => self.theme.cursor,
            _ => indexed_color(&self.theme, idx as u8),
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

}

/// Map a NamedColor to the theme.
fn named_color(theme: &Theme, n: NamedColor) -> Rgba {
    let i = n as usize;
    match i {
        0..=15 => theme.ansi[i],
        256 => theme.fg,
        257 => theme.bg,
        258 => theme.cursor,
        259..=266 => dim(theme.ansi[i - 259]),
        267 => theme.fg,
        268 => dim(theme.fg),
        _ => theme.fg,
    }
}

/// The 256-color palette: 16 ANSI, a 6x6x6 cube, and a 24-step gray ramp.
fn indexed_color(theme: &Theme, i: u8) -> Rgba {
    match i {
        0..=15 => theme.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
            let f = |v: u8| if v == 0 { 0.0 } else { (55 + v * 40) as f32 / 255.0 };
            [f(r), f(g), f(b), 1.0]
        }
        232..=255 => {
            let v = (8 + (i - 232) as u32 * 10) as f32 / 255.0;
            [v, v, v, 1.0]
        }
    }
}

fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~")
        && let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), rest);
        }
    p.to_string()
}

/// Tiny helper so wheel-to-arrow can swap SS3 for CSI.
trait ReplaceO {
    fn replace_o(&self) -> Vec<u8>;
}
impl ReplaceO for [u8] {
    fn replace_o(&self) -> Vec<u8> {
        let mut v = self.to_vec();
        if v.len() > 1 && v[1] == b'O' {
            v[1] = b'[';
        }
        v
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.wins.is_empty() {
            return;
        }
        if self.create_window(event_loop, Vec::new()).is_none() {
            event_loop.exit();
            return;
        }

        if std::env::args().any(|a| a == "--deck") {
            self.win_mut().deck.open(PageId::Home);
        }

        if let Some(input) = self.debug.input.clone()
            && let Some(tab) = self.win().tabs.first() {
                tab.write(input.into_bytes());
            }
        if let Some(ms) = self.debug.exit_after {
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                let _ = proxy.send_event(UserEvent { tab: 0, event: Event::Exit });
            });
        }
        if let Some(ms) = self.debug.actions_after {
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                let _ = proxy.send_event(UserEvent { tab: 0, event: Event::Wakeup });
            });
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Fired when a WaitUntil deadline passes (and after every batch of
        // events): expire status messages and tick animations.
        let now = Instant::now();
        for w in &mut self.wins {
            if w.status.as_ref().map(|(_, at)| at.elapsed().as_secs() >= 4).unwrap_or(false) {
                w.status = None;
                w.window.request_redraw();
            }
        }
        if self.next_frame.map(|t| now >= t).unwrap_or(false) {
            self.next_frame = None;
            let anim = self.config.terminal.cursor_animation.clone();
            for w in &self.wins {
                if Self::window_animating(w, &anim) {
                    w.window.request_redraw();
                }
            }
        }
        self.schedule_wakeups(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if event.tab == crate::terminal::SYS_DROP_TIMEOUT {
            self.resolve_pending_drop(None, event_loop);
            return;
        }
        if event.tab == 0 {
            // Reserved id used by the debug timers.
            self.cur = 0;
            match event.event {
                Event::Wakeup => self.run_debug_actions(Some(event_loop)),
                _ => {
                    self.take_debug_screenshot();
                    event_loop.exit();
                }
            }
            return;
        }
        let Some(wi) = self.window_of_tab(event.tab) else { return };
        self.cur = wi;
        self.on_term_event(event.tab, event.event, event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(wi) = self.window_index(id) else { return };
        self.cur = wi;
        if let Some(p) = self.pending_drop.as_ref() {
            let other = p.from != id;
            let pointer_here = matches!(event, WindowEvent::CursorEntered { .. } | WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } | WindowEvent::Focused(true));
            log::debug!("event while drop pending (other window: {other}): {event:?}");
            if other && pointer_here {
                self.resolve_pending_drop(Some(id), event_loop);
                return;
            }
        }
        match event {
            WindowEvent::CloseRequested => self.close_window(wi, event_loop),
            WindowEvent::Resized(size) => {
                self.win_mut().renderer.resize(size.width, size.height);
                self.relayout();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let lp = self.config.font.line_padding;
                let pt = self.font_pt;
                let w = self.win_mut();
                w.scale = scale_factor;
                w.fonts.set_size(pt * scale_factor as f32, lp * scale_factor as f32);
                self.relayout();
            }
            WindowEvent::CursorEntered { .. } => {}
            WindowEvent::ModifiersChanged(m) => {
                let new = m.state();
                self.mods = new;
                let both = new.control_key() && new.shift_key() && !new.alt_key() && !new.super_key();
                if both {
                    if self.chord_armed.is_none() {
                        self.chord_armed = Some(Instant::now());
                    }
                } else if let Some(t) = self.chord_armed.take() {
                    // Released cleanly within a tap window: toggle the Deck.
                    if self.config.input.ctrl_shift_tap_opens_deck && t.elapsed().as_millis() < 700 && self.wins[self.cur].menu.is_none() && self.wins[self.cur].palette.is_none() {
                        let a = self.wins[self.cur].deck.toggle();
                        self.apply_deck_action(a);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, event_loop),
            WindowEvent::CursorMoved { position, .. } => {
                self.wins[self.cur].mouse = position;
                self.on_mouse_move();
            }
            WindowEvent::CursorLeft { .. } => {
                if self.wins[self.cur].hover != Hover::None {
                    self.wins[self.cur].hover = Hover::None;
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(state, button, event_loop),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::Focused(f) => {
                self.wins[self.cur].focused = f;
                if f {
                    if let Some(v) = self.wins[self.cur].view_mut() {
                        v.cursor_anim.pulse_start = Some(Instant::now());
                    }
                }
                if let Some(tab) = self.wins[self.cur].tabs.get(self.wins[self.cur].active)
                    && tab.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                        tab.write(if f { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
                    }
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let t0 = Instant::now();
                if let Err(e) = self.draw() {
                    log::error!("draw failed: {e:#}");
                }
                let dt = t0.elapsed().as_secs_f64() * 1e3;
                if dt > 4.0 {
                    log::debug!("frame {:.1}ms (batch {} instances)", dt, self.wins[self.cur].batch.instances.len());
                }
                self.schedule_wakeups(event_loop);
            }
            _ => {}
        }
    }
}
