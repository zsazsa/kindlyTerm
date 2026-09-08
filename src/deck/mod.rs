//! The Control Deck: a slide-in sidebar holding the shortcut launcher and
//! the settings system. Implements the "kindterm Control Deck" design.
//!
//! - `pages.rs` builds the widget list for each page
//! - `input.rs` keyboard and mouse handling
//! - `draw.rs`  layout and drawing, plus the UI text helpers
//!
//! The Deck owns only UI state (page stack, focus, filter text, editor
//! draft). Everything it wants to change in the app is returned as a
//! [`DeckAction`] for `App` to apply, which keeps the borrow story simple.

mod draw;
mod input;
mod pages;

pub use draw::{ui_text, ui_text_tracked_pub, ui_text_width};

use std::time::Instant;

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::config::{Config, DeckState, SavedCommand};
use crate::effects::{EffectChange, EffectsConfig};
use crate::font::{FontSystem, GlyphKey};
use crate::renderer::{Batch, Rgba, scale_rgb, with_alpha};
use crate::theme::{BADGE_COLORS, BUILTIN_THEMES, ICON_SHEET, Theme, guess_icon};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchTarget {
    /// Whatever the shortcut's saved target is.
    Saved,
    NewTab,
    Here,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipField {
    CopyOnSelect,
    CtrlC,
    CtrlV,
    TrimNewline,
}

#[derive(Debug, Clone)]
pub enum DeckAction {
    None,
    Launch { name: String, target: LaunchTarget },
    RunRaw(String),
    PreviewTheme(Option<String>),
    ApplyTheme(String),
    PreviewFont(Option<String>),
    ApplyFont(String),
    SetFontSize(f32),
    SetLinePadding(f32),
    SetPadding(f32),
    SetOpacity(f32),
    Effect(EffectChange),
    SetCursor(String),
    SetScrollback(usize),
    SetShell(Option<String>),
    SetClipboard(ClipField, bool),
    SaveShortcut { cmd: SavedCommand, previous: Option<String> },
    DeleteShortcut(String),
    ReloadCommands,
}

/// Read-only view of app state the Deck needs to build its pages.
pub struct DeckEnv<'a> {
    pub config: &'a Config,
    pub shortcuts: &'a [SavedCommand],
    pub state: &'a DeckState,
    pub theme: &'a Theme,
    pub font_families: &'a [String],
    /// Resolved family actually loaded (config may say "monospace").
    pub font_family: &'a str,
    pub tab_count: usize,
    pub gpu: &'a str,
    pub scale: f32,
    pub effects: &'a EffectsConfig,
}

// ---------------------------------------------------------------------------
// Pages, widgets, focus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageId {
    Home,
    Theme,
    Font,
    Padding,
    Opacity,
    Effects,
    Cursor,
    Shell,
    Scrollback,
    Tabs,
    Keyboard,
    Clipboard,
    Manage,
    ImportExport,
    About,
    Editor,
}

struct PageState {
    id: PageId,
    focus: usize,
    scroll: f32,
    filter: String,
    entered: Instant,
    /// Scroll to reveal the focused item on the next frame. Set when focus
    /// moves; cleared after use so wheel scrolling is never fought.
    reveal_focus: bool,
}

impl PageState {
    fn new(id: PageId) -> Self {
        Self { id, focus: 0, scroll: 0.0, filter: String::new(), entered: Instant::now(), reveal_focus: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldId {
    Name,
    Command,
    Cwd,
    Hotkey,
    Shell,
}

/// What activating / adjusting a focusable does.
#[derive(Clone)]
enum Act {
    None,
    Push(PageId),
    Pop,
    App(DeckAction),
    Edit(Option<String>),
    EditorSave,
    EditorDelete,
    EditorBind,
    EditorNext,
    EditorGlyph(i32),
    EditorColor(i32),
    EditorToggle(FieldId),
    EditorRunIn,
    ShellCommit,
}

/// A focusable item as produced by the page builder.
struct Focusable {
    enter: Act,
    left: Act,
    right: Act,
    /// Text field that receives typing while focused.
    field: Option<FieldId>,
    /// Grid membership (start index, length, columns) for tile navigation.
    grid: Option<(usize, usize, usize)>,
    /// Shortcut name, for modifier-Enter launch variants.
    shortcut: Option<String>,
}

#[derive(Clone)]
struct Badge {
    glyph: char,
    color: u8,
}

#[derive(Clone)]
enum RowKind {
    Chevron,
    Toggle(bool),
    Stepper(String),
    Check(bool),
    Info,
    Hint(String),
}

#[derive(Clone)]
struct Row {
    badge: Option<Badge>,
    title: String,
    /// Character positions in `title` to highlight (fuzzy match).
    hi: Vec<usize>,
    subtitle: String,
    value: String,
    kind: RowKind,
    enabled: bool,
    danger: bool,
    focus: Option<usize>,
}

#[derive(Clone)]
struct Tile {
    badge: Badge,
    label: String,
    focus: usize,
}

enum W {
    Header { back: String, title: String, hint: String },
    Search { text: String, placeholder: String, right: String },
    Label { text: String, right: String },
    Tiles(Vec<Tile>),
    Group(Vec<Row>),
    LetterHeader(String),
    ThemeCard { idx: usize, selected: bool, focus: usize },
    FontPreview,
    Field { label: String, value: String, placeholder: String, focus: usize, multiline: bool, active: bool, right: String },
    GlyphPicker { focus_glyph: usize, focus_color: usize },
    Buttons { cancel: String, save: String, focus: usize },
    Note(String),
    CommandStrip(String),
}

struct Built {
    widgets: Vec<W>,
    focusables: Vec<Focusable>,
}

impl Built {
    fn add(&mut self, f: Focusable) -> usize {
        self.focusables.push(f);
        self.focusables.len() - 1
    }
    fn simple(&mut self, enter: Act) -> usize {
        self.add(Focusable { enter, left: Act::None, right: Act::None, field: None, grid: None, shortcut: None })
    }
    fn adjustable(&mut self, enter: Act, left: Act, right: Act) -> usize {
        self.add(Focusable { enter, left, right, field: None, grid: None, shortcut: None })
    }
    fn field(&mut self, id: FieldId, enter: Act) -> usize {
        self.add(Focusable { enter, left: Act::None, right: Act::None, field: Some(id), grid: None, shortcut: None })
    }
}

// ---------------------------------------------------------------------------
// Editor draft
// ---------------------------------------------------------------------------

struct Editor {
    previous: Option<String>,
    draft: SavedCommand,
    glyph_idx: usize,
    color_idx: usize,
    binding: bool,
}

impl Editor {
    fn new(previous: Option<&SavedCommand>, prefill_command: Option<String>) -> Self {
        let draft = match previous {
            Some(c) => c.clone(),
            None => SavedCommand::new(String::new(), prefill_command.unwrap_or_default()),
        };
        let (g, col) = badge_for(&draft);
        let glyph_idx = ICON_SHEET.iter().position(|(c, _, _)| *c == g).unwrap_or(0);
        let color_idx = BADGE_COLORS.iter().position(|c| *c == col).unwrap_or(3);
        Self { previous: previous.map(|c| c.name.clone()), draft, glyph_idx, color_idx, binding: false }
    }

    fn sync_badge(&mut self) {
        self.draft.icon = Some(ICON_SHEET[self.glyph_idx].0.to_string());
        self.draft.color = Some(BADGE_COLORS[self.color_idx]);
    }
}

/// Badge for a shortcut: explicit icon/color, else a guess from the command.
fn badge_for(c: &SavedCommand) -> (char, u8) {
    let (gg, gc) = guess_icon(&c.name, &c.command);
    let glyph = c.icon.as_ref().and_then(|s| s.chars().next()).unwrap_or(gg);
    let color = c.color.unwrap_or(gc).min(15);
    (glyph, color)
}

// ---------------------------------------------------------------------------
// Fuzzy ranking (design: exact, prefix, initials, subsequence, host, body)
// ---------------------------------------------------------------------------

struct Match {
    tier: u8,
    positions: Vec<usize>,
}

fn subsequence(hay: &[char], needle: &[char]) -> Option<Vec<usize>> {
    let mut pos = Vec::with_capacity(needle.len());
    let mut i = 0;
    for &n in needle {
        while i < hay.len() && hay[i].to_ascii_lowercase() != n {
            i += 1;
        }
        if i == hay.len() {
            return None;
        }
        pos.push(i);
        i += 1;
    }
    Some(pos)
}

fn rank(query: &str, cmd: &SavedCommand) -> Option<Match> {
    let q: Vec<char> = query.trim().to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(Match { tier: 9, positions: vec![] });
    }
    let name: Vec<char> = cmd.name.chars().collect();
    let lname: Vec<char> = name.iter().map(|c| c.to_ascii_lowercase()).collect();
    if lname == q {
        return Some(Match { tier: 0, positions: (0..q.len()).collect() });
    }
    if lname.starts_with(&q) {
        return Some(Match { tier: 1, positions: (0..q.len()).collect() });
    }
    // Word-boundary initials: "pdp" -> prod-db-primary.
    let mut initials = Vec::new();
    for (i, &c) in lname.iter().enumerate() {
        if i == 0 || matches!(lname[i - 1], '-' | '_' | ' ' | '.' | '/') {
            initials.push((c, i));
        }
    }
    let ini: Vec<char> = initials.iter().map(|(c, _)| *c).collect();
    if ini.starts_with(&q) {
        return Some(Match { tier: 2, positions: initials.iter().take(q.len()).map(|(_, i)| *i).collect() });
    }
    if let Some(p) = subsequence(&name, &q) {
        return Some(Match { tier: 3, positions: p });
    }
    if let Some(h) = cmd.host()
        && h.to_lowercase().contains(&query.trim().to_lowercase()) {
            return Some(Match { tier: 4, positions: vec![] });
        }
    let body: Vec<char> = cmd.command.chars().collect();
    if subsequence(&body, &q).is_some() {
        return Some(Match { tier: 5, positions: vec![] });
    }
    None
}

// ---------------------------------------------------------------------------
// Layout constants (design px at scale 1)
// ---------------------------------------------------------------------------

const PANEL_W: f32 = 392.0;
const PAD: f32 = 18.0;
const ROW_H: f32 = 32.0;
const TALL_ROW_H: f32 = 42.0;
const HEADER_H: f32 = 44.0;
const SEARCH_H: f32 = 36.0;
const BADGE: f32 = 26.0;
const TILE: f32 = 52.0;
const TILE_COLS: usize = 5;
/// Panel text is drawn a touch larger than the design's px values.
const TEXT_SCALE: f32 = 1.12;
const LABEL_H: f32 = 24.0;
const GROUP_GAP: f32 = 14.0;
const TILE_ROW_H: f32 = TILE + 8.0 + 14.0 + 12.0;
const RADIUS_ROW: f32 = 4.0;
const RADIUS_BADGE: f32 = 6.0;
const SLIDE_MS: f32 = 160.0;
const PUSH_MS: f32 = 120.0;

// ---------------------------------------------------------------------------
// The Deck
// ---------------------------------------------------------------------------

pub struct Deck {
    open: bool,
    anim_start: Option<Instant>,
    stack: Vec<PageState>,
    editor: Option<Editor>,
    theme_preview: bool,
    font_preview: bool,
    /// Draft for the Shell page.
    shell_draft: Option<String>,
    /// Focus rects from the last draw, for mouse hit-testing.
    rects: Vec<(usize, Rect)>,
    hover: Option<usize>,
    /// Content viewport from the last draw.
    content_rect: Rect,
    panel_rect: Rect,
    gear_rect: Rect,
    /// Footer theme swatches drawn last frame: (theme name, rect).
    swatch_rects: Vec<(&'static str, Rect)>,
    hover_swatch: Option<&'static str>,
    /// Cached build for hit-testing between frames.
    focus_count: usize,
    focus_shortcuts: Vec<Option<String>>,
    focus_grid: Vec<Option<(usize, usize, usize)>>,
    focus_fields: Vec<Option<FieldId>>,
    pub toast: Option<(String, Instant)>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

impl Deck {
    pub fn new() -> Self {
        Self {
            open: false,
            anim_start: None,
            stack: vec![PageState::new(PageId::Home)],
            editor: None,
            theme_preview: false,
            font_preview: false,
            shell_draft: None,
            rects: Vec::new(),
            hover: None,
            content_rect: Rect::default(),
            panel_rect: Rect::default(),
            gear_rect: Rect::default(),
            swatch_rects: Vec::new(),
            hover_swatch: None,
            focus_count: 0,
            focus_shortcuts: Vec::new(),
            focus_grid: Vec::new(),
            focus_fields: Vec::new(),
            toast: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// True while the slide or a page push is still animating.
    pub fn animating(&self) -> bool {
        if let Some(t) = self.anim_start
            && t.elapsed().as_secs_f32() * 1000.0 < SLIDE_MS {
                return true;
            }
        self.stack.last().map(|p| p.entered.elapsed().as_secs_f32() * 1000.0 < PUSH_MS).unwrap_or(false)
            || self.toast.as_ref().map(|(_, t)| t.elapsed().as_secs() < 3).unwrap_or(false)
    }

    /// 0 = fully closed, 1 = fully open (eased).
    fn slide(&self) -> f32 {
        let t = self
            .anim_start
            .map(|s| (s.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS).min(1.0))
            .unwrap_or(1.0);
        let eased = 1.0 - (1.0 - t) * (1.0 - t);
        if self.open { eased } else { 1.0 - eased }
    }

    /// True if the panel should still be drawn (open, or sliding out).
    pub fn visible(&self) -> bool {
        self.open || self.slide() > 0.0
    }

    pub fn open(&mut self, page: PageId) {
        if !self.open {
            self.open = true;
            self.anim_start = Some(Instant::now());
        }
        self.stack.clear();
        self.stack.push(PageState::new(PageId::Home));
        if page != PageId::Home {
            self.stack.push(PageState::new(page));
        }
        self.hover = None;
    }

    /// Toggle open/closed. Returns the revert action if a preview was live.
    pub fn toggle(&mut self) -> DeckAction {
        if self.open {
            self.close()
        } else {
            self.open(PageId::Home);
            DeckAction::None
        }
    }

    pub fn close(&mut self) -> DeckAction {
        if !self.open {
            return DeckAction::None;
        }
        self.open = false;
        self.anim_start = Some(Instant::now());
        self.editor = None;
        self.shell_draft = None;
        let mut action = DeckAction::None;
        if self.theme_preview {
            self.theme_preview = false;
            action = DeckAction::PreviewTheme(None);
        }
        if self.font_preview {
            self.font_preview = false;
            action = DeckAction::PreviewFont(None);
        }
        action
    }

    /// Open the editor for a new shortcut (optionally prefilled) or an
    /// existing one by name.
    pub fn edit_shortcut(&mut self, existing: Option<&SavedCommand>, prefill: Option<String>) {
        self.open(PageId::Manage);
        self.editor = Some(Editor::new(existing, prefill));
        self.stack.push(PageState::new(PageId::Editor));
        // Focus the name field (index 2, after the glyph and color pickers).
        if let Some(p) = self.stack.last_mut() {
            p.focus = 2;
            p.reveal_focus = true;
        }
    }

    pub fn set_toast(&mut self, msg: String) {
        self.toast = Some((msg, Instant::now()));
    }

    fn page(&self) -> &PageState {
        self.stack.last().expect("page stack never empty")
    }

    fn page_mut(&mut self) -> &mut PageState {
        self.stack.last_mut().expect("page stack never empty")
    }

    fn push(&mut self, id: PageId) {
        self.stack.push(PageState::new(id));
    }

    fn pop(&mut self) -> DeckAction {
        let mut action = DeckAction::None;
        if let Some(p) = self.stack.pop() {
            match p.id {
                PageId::Theme if self.theme_preview => {
                    self.theme_preview = false;
                    action = DeckAction::PreviewTheme(None);
                }
                PageId::Font if self.font_preview => {
                    self.font_preview = false;
                    action = DeckAction::PreviewFont(None);
                }
                PageId::Editor => self.editor = None,
                PageId::Shell => self.shell_draft = None,
                _ => {}
            }
        }
        if self.stack.is_empty() {
            self.stack.push(PageState::new(PageId::Home));
        }
        action
    }

}


impl Default for Deck {
    fn default() -> Self {
        Self::new()
    }
}

enum TextEdit {
    Insert(String),
    Backspace,
    Clear,
    DeleteWord,
}

fn footer_hints(id: PageId) -> (&'static str, &'static str) {
    match id {
        PageId::Theme => ("↑↓ move · ⏎ apply", "live preview behind panel"),
        PageId::Font => ("←→ step · ⏎ apply font", "applies live"),
        PageId::Manage => ("⏎ edit · ⌥⏎ launch", "Ctrl+Shift+S new"),
        PageId::Editor => ("Esc cancel", "Ctrl+S save"),
        PageId::Shell => ("⏎ save", "Esc back"),
        _ => ("↑↓ move · ⏎ select", "Esc back"),
    }
}

fn info_row(title: &str, value: &str) -> Row {
    Row {
        badge: None,
        title: title.to_string(),
        hi: vec![],
        subtitle: String::new(),
        value: value.to_string(),
        kind: RowKind::Info,
        enabled: true,
        danger: false,
        focus: None,
    }
}

fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

fn ago(unix: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let d = now.saturating_sub(unix);
    if d < 60 {
        "just now".into()
    } else if d < 3600 {
        format!("{} m ago", d / 60)
    } else if d < 86400 {
        format!("{} h ago", d / 3600)
    } else {
        format!("{} d ago", d / 86400)
    }
}

/// Human-readable hotkey name for a key press, e.g. "Alt+H", "Ctrl+Alt+F5".
pub fn hotkey_name(base: &Key, mods: ModifiersState) -> Option<String> {
    let key = match base {
        Key::Character(c) => c.to_uppercase(),
        Key::Named(n) => match n {
            NamedKey::F1 => "F1".into(),
            NamedKey::F2 => "F2".into(),
            NamedKey::F3 => "F3".into(),
            NamedKey::F4 => "F4".into(),
            NamedKey::F5 => "F5".into(),
            NamedKey::F6 => "F6".into(),
            NamedKey::F7 => "F7".into(),
            NamedKey::F8 => "F8".into(),
            NamedKey::F9 => "F9".into(),
            NamedKey::F10 => "F10".into(),
            NamedKey::F11 => "F11".into(),
            NamedKey::F12 => "F12".into(),
            NamedKey::Home => "Home".into(),
            NamedKey::End => "End".into(),
            NamedKey::Insert => "Insert".into(),
            NamedKey::PageUp => "PageUp".into(),
            NamedKey::PageDown => "PageDown".into(),
            _ => return None,
        },
        _ => return None,
    };
    // A bare letter without a modifier would shadow typing; require one.
    if !mods.control_key() && !mods.alt_key() && !mods.super_key() && !key.starts_with('F') {
        return None;
    }
    // Never let a hotkey steal the shell's interrupt, EOF, or suspend keys.
    if mods.control_key() && !mods.alt_key() && !mods.super_key() && !mods.shift_key() && matches!(key.as_str(), "C" | "D" | "Z") {
        return None;
    }
    let mut parts = Vec::new();
    if mods.control_key() {
        parts.push("Ctrl");
    }
    if mods.alt_key() {
        parts.push("Alt");
    }
    if mods.shift_key() {
        parts.push("Shift");
    }
    if mods.super_key() {
        parts.push("Super");
    }
    parts.push(&key);
    Some(parts.join("+"))
}

/// Does a key press match a hotkey string produced by `hotkey_name`?
pub fn hotkey_matches(hotkey: &str, base: &Key, mods: ModifiersState) -> bool {
    hotkey_name(base, mods).map(|h| h.eq_ignore_ascii_case(hotkey)).unwrap_or(false)
}
