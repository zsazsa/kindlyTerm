//! Visual effects: the typing trail and the paste rain. Configuration lives
//! in `~/.config/kindlyterm/effects.toml` so a look can be shared by copying
//! one file; the runtime state is per window.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config::{config_dir, parse_hex};
use crate::font::{FontSystem, GlyphKey};
use crate::renderer::{Batch, Rgba, rgb, with_alpha};
use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Configuration (effects.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EffectsConfig {
    /// Name of the preset these values came from ("custom" once edited).
    pub preset: String,
    pub typing_trail: TrailConfig,
    pub paste_rain: RainConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TrailConfig {
    pub enabled: bool,
    /// How many cells behind the cursor keep glowing.
    pub length: usize,
    /// How long one cell glows, in milliseconds.
    pub fade_ms: u32,
    /// Typing speed (chars/second) at which the trail reaches full strength.
    pub full_speed_cps: f32,
    /// "neon" (cursor color fading to accent), "accent", "cursor", "green",
    /// or a "#rrggbb" color.
    pub color: String,
    /// Draw a soft glow box behind each trail glyph.
    pub glow: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RainConfig {
    pub enabled: bool,
    /// Pastes shorter than this do not trigger the rain.
    pub min_chars: usize,
    pub duration_ms: u32,
    /// "text" (the pasted characters), "katakana", "ascii", or "binary".
    pub glyphs: String,
    /// "accent", "green", "cursor", or "#rrggbb".
    pub color: String,
    /// Share of pasted characters that are animated, 0.1..1.0 (the rest
    /// simply appear). Also keeps very large pastes cheap.
    pub density: f32,
    /// Trailing glyphs behind each drop's head.
    pub tail: usize,
}

impl Default for EffectsConfig {
    fn default() -> Self {
        preset("cyberpunk").unwrap()
    }
}

impl Default for TrailConfig {
    fn default() -> Self {
        Self { enabled: true, length: 12, fade_ms: 450, full_speed_cps: 5.0, color: "neon".into(), glow: true }
    }
}

impl Default for RainConfig {
    fn default() -> Self {
        Self { enabled: true, min_chars: 40, duration_ms: 700, glyphs: "text".into(), color: "accent".into(), density: 1.0, tail: 6 }
    }
}

pub const PRESETS: &[&str] = &["off", "subtle", "cyberpunk", "matrix"];

pub fn preset(name: &str) -> Option<EffectsConfig> {
    let (trail, rain) = match name {
        "off" => (
            TrailConfig { enabled: false, ..TrailConfig::default() },
            RainConfig { enabled: false, ..RainConfig::default() },
        ),
        "subtle" => (
            TrailConfig { enabled: true, length: 6, fade_ms: 300, full_speed_cps: 7.0, color: "accent".into(), glow: false },
            RainConfig { enabled: true, min_chars: 200, duration_ms: 500, glyphs: "text".into(), color: "accent".into(), density: 0.5, tail: 4 },
        ),
        "cyberpunk" => (TrailConfig::default(), RainConfig::default()),
        "matrix" => (
            TrailConfig { enabled: true, length: 16, fade_ms: 650, full_speed_cps: 4.0, color: "green".into(), glow: true },
            RainConfig { enabled: true, min_chars: 20, duration_ms: 950, glyphs: "katakana".into(), color: "green".into(), density: 1.0, tail: 10 },
        ),
        _ => return None,
    };
    Some(EffectsConfig { preset: name.to_string(), typing_trail: trail, paste_rain: rain })
}

impl EffectsConfig {
    pub fn path() -> PathBuf {
        config_dir().join("effects.toml")
    }

    /// Load effects.toml, writing the default preset if it does not exist.
    pub fn load() -> Self {
        match fs::read_to_string(Self::path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::error!("effects.toml: {e}; using defaults");
                Self::default()
            }),
            Err(_) => {
                let cfg = Self::default();
                let _ = cfg.save();
                cfg
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = Self::path().parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)?;
        let text = format!(
            "# kindlyTerm effects. Share this file to share the look.\n\
             # preset: off | subtle | cyberpunk | matrix (editing any value makes it \"custom\")\n\
             # typing_trail.color / paste_rain.color: neon | accent | cursor | green | #rrggbb\n\
             # paste_rain.glyphs: text | katakana | ascii | binary\n\n{body}"
        );
        fs::write(Self::path(), text)?;
        Ok(())
    }

    /// Mark as edited unless the values still equal a named preset.
    pub fn refresh_preset_name(&mut self) {
        for p in PRESETS {
            if let Some(cfg) = preset(p)
                && cfg.typing_trail == self.typing_trail && cfg.paste_rain == self.paste_rain {
                    self.preset = p.to_string();
                    return;
                }
        }
        self.preset = "custom".into();
    }

}

/// One editable knob on the Deck's Effects page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectChange {
    PresetStep(i32),
    TrailToggle,
    TrailLength(i32),
    TrailFade(i32),
    TrailColorNext,
    TrailGlowToggle,
    RainToggle,
    RainThreshold(i32),
    RainDuration(i32),
    RainGlyphsNext,
    RainColorNext,
    RainDensity(i32),
    Reload,
}

const COLORS: &[&str] = &["neon", "accent", "cursor", "green"];
const GLYPH_SETS: &[&str] = &["text", "katakana", "ascii", "binary"];

fn cycle(list: &[&str], cur: &str) -> String {
    let i = list.iter().position(|c| c.eq_ignore_ascii_case(cur)).map(|i| (i + 1) % list.len()).unwrap_or(0);
    list[i].to_string()
}

impl EffectsConfig {
    pub fn apply(&mut self, change: EffectChange) {
        match change {
            EffectChange::PresetStep(d) => {
                let cur = PRESETS.iter().position(|p| *p == self.preset).unwrap_or(2) as i32;
                let next = (cur + d).rem_euclid(PRESETS.len() as i32) as usize;
                if let Some(cfg) = preset(PRESETS[next]) {
                    *self = cfg;
                }
                return;
            }
            EffectChange::TrailToggle => self.typing_trail.enabled = !self.typing_trail.enabled,
            EffectChange::TrailLength(d) => self.typing_trail.length = (self.typing_trail.length as i32 + d).clamp(2, 40) as usize,
            EffectChange::TrailFade(d) => self.typing_trail.fade_ms = (self.typing_trail.fade_ms as i32 + d * 50).clamp(100, 2000) as u32,
            EffectChange::TrailColorNext => self.typing_trail.color = cycle(COLORS, &self.typing_trail.color),
            EffectChange::TrailGlowToggle => self.typing_trail.glow = !self.typing_trail.glow,
            EffectChange::RainToggle => self.paste_rain.enabled = !self.paste_rain.enabled,
            EffectChange::RainThreshold(d) => self.paste_rain.min_chars = (self.paste_rain.min_chars as i32 + d * 10).clamp(0, 2000) as usize,
            EffectChange::RainDuration(d) => self.paste_rain.duration_ms = (self.paste_rain.duration_ms as i32 + d * 100).clamp(200, 3000) as u32,
            EffectChange::RainGlyphsNext => self.paste_rain.glyphs = cycle(GLYPH_SETS, &self.paste_rain.glyphs),
            EffectChange::RainColorNext => self.paste_rain.color = cycle(&COLORS[1..], &self.paste_rain.color),
            EffectChange::RainDensity(d) => self.paste_rain.density = (self.paste_rain.density + d as f32 * 0.1).clamp(0.1, 1.0),
            EffectChange::Reload => {
                *self = Self::load();
                return;
            }
        }
        self.refresh_preset_name();
    }
}

// ---------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------

struct TrailGlyph {
    ch: char,
    x: f32,
    y: f32,
    at: Instant,
    /// 0..1 strength from typing speed at the time.
    intensity: f32,
}

/// One pasted character falling into the cell where it really landed.
struct Fall {
    col: usize,
    row: usize,
    ch: char,
    start: Instant,
    /// Seconds to wait before this glyph starts falling.
    delay: f32,
    /// Seconds the fall itself takes.
    dur: f32,
    seed: u32,
}

/// Visible grid captured right before a paste is sent to the shell.
pub struct GridSnapshot {
    pub cells: Vec<char>,
    pub cols: usize,
    pub rows: usize,
    /// Scrollback length at capture time; growth = how far the screen
    /// scrolled since, so rows can be compared against the right line.
    pub history: usize,
}

struct Pending {
    snap: GridSnapshot,
    density: f32,
    /// Characters of the paste still unaccounted for (multiset).
    remaining: std::collections::HashMap<char, usize>,
    deadline: Instant,
    duration: f32,
    /// First landing row seen; later rows get proportionally longer delays.
    first_row: Option<usize>,
}

/// Tiny xorshift PRNG so we do not need a crate for confetti.
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

pub struct Effects {
    trail: VecDeque<TrailGlyph>,
    falls: Vec<Fall>,
    /// Cells whose real glyph is hidden until its drop lands.
    masked: std::collections::HashSet<(usize, usize)>,
    pending: Option<Pending>,
    /// Glyphs used for the shimmer of falls (kept after detection ends).
    shimmer: Vec<char>,
    rng: Rng,
}

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

impl Effects {
    pub fn new() -> Self {
        Self { trail: VecDeque::new(), falls: Vec::new(), masked: std::collections::HashSet::new(), pending: None, shimmer: Vec::new(), rng: Rng(0x9e37_79b9) }
    }

    /// Anything still animating?
    pub fn active(&self) -> bool {
        !self.trail.is_empty() || !self.falls.is_empty() || self.pending.is_some()
    }

    /// Is a paste being watched for landing cells?
    pub fn watching(&self) -> bool {
        self.pending.is_some()
    }

    /// Should this cell's real glyph be hidden (its drop is still in the air)?
    pub fn is_masked(&self, col: usize, row: usize) -> bool {
        !self.masked.is_empty() && self.masked.contains(&(col, row))
    }

    /// Record a typed character at the cell where it will appear.
    pub fn typed(&mut self, cfg: &TrailConfig, ch: char, x: f32, y: f32) {
        if !cfg.enabled || ch.is_control() {
            return;
        }
        let now = Instant::now();
        // Typing speed over the last second, including this key.
        let recent = self.trail.iter().filter(|g| now.duration_since(g.at).as_secs_f32() < 1.0).count() + 1;
        let cps = recent as f32;
        let full = cfg.full_speed_cps.max(1.5);
        let intensity = ((cps - 1.5) / (full - 1.5)).clamp(0.0, 1.0);
        self.trail.push_back(TrailGlyph { ch, x, y, at: now, intensity });
        while self.trail.len() > cfg.length.max(1) {
            self.trail.pop_front();
        }
    }

    /// Arm the paste rain: remember what the screen looked like before the
    /// paste so the cells it changes can be found on the next frames.
    pub fn begin_paste(&mut self, cfg: &RainConfig, text: &str, snap: GridSnapshot) {
        if !cfg.enabled || text.chars().count() < cfg.min_chars {
            return;
        }
        let mut remaining = std::collections::HashMap::new();
        for c in text.chars().filter(|c| !c.is_whitespace() && !c.is_control()) {
            *remaining.entry(c).or_insert(0usize) += 1;
        }
        if remaining.is_empty() {
            return;
        }
        let glyph_set: Vec<char> = match cfg.glyphs.as_str() {
            "katakana" => "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789".chars().collect(),
            "ascii" => "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789<>/\\|{}[]#$%&*+=".chars().collect(),
            "binary" => "01".chars().collect(),
            _ => {
                // "text": shimmer through the paste's own characters.
                let mut v: Vec<char> = remaining.keys().copied().collect();
                v.sort_unstable();
                v
            }
        };
        let now = Instant::now();
        self.shimmer = glyph_set;
        self.pending = Some(Pending {
            snap,
            density: cfg.density.clamp(0.1, 1.0),
            remaining,
            deadline: now + std::time::Duration::from_millis(400),
            duration: cfg.duration_ms.max(200) as f32 / 1000.0,
            first_row: None,
        });
    }

    /// Called for every visible cell while a paste is being watched. A cell
    /// whose character changed to one from the paste becomes a landing spot.
    pub fn observe_cell(&mut self, col: usize, row: usize, ch: char, history_now: usize) {
        let Some(p) = self.pending.as_mut() else { return };
        if ch == ' ' || ch.is_control() {
            return;
        }
        let Some(left) = p.remaining.get_mut(&ch) else { return };
        if *left == 0 {
            return;
        }
        // The screen may have scrolled since the snapshot: compare with the
        // line that used to be at this position.
        let scrolled = history_now.saturating_sub(p.snap.history);
        let old_row = row + scrolled;
        let old = if old_row < p.snap.rows && col < p.snap.cols { p.snap.cells[old_row * p.snap.cols + col] } else { ' ' };
        if old == ch {
            return;
        }
        if self.masked.contains(&(col, row)) {
            return;
        }
        *left -= 1;
        // Skip some characters (density) and cap the total so a 10k-char
        // paste does not spawn 10k drops.
        if self.rng.unit() > p.density || self.falls.len() >= 900 {
            if p.remaining.values().all(|n| *n == 0) {
                self.pending = None;
            }
            return;
        }
        let first = *p.first_row.get_or_insert(row);
        // Earlier rows land first; a little jitter keeps it organic.
        let row_frac = ((row.saturating_sub(first)) as f32 / 24.0).min(1.0);
        let delay = row_frac * p.duration * 0.45 + self.rng.unit() * p.duration * 0.25;
        let dur = p.duration * (0.35 + 0.25 * self.rng.unit());
        self.falls.push(Fall { col, row, ch, start: Instant::now(), delay, dur, seed: self.rng.next() });
        self.masked.insert((col, row));
        if p.remaining.values().all(|n| *n == 0) {
            self.pending = None;
        }
    }

    /// Draw both effects. Call after the grid and before the cursor overlay.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, cfg: &EffectsConfig, fonts: &mut FontSystem, batch: &mut Batch, theme: &Theme, grid_x: f32, grid_y: f32, zoom: f32, now: Instant) {
        let base_px = fonts.size_px;
        let m0 = fonts.metrics;
        let m = crate::font::CellMetrics {
            width: m0.width * zoom,
            height: m0.height * zoom,
            ascent: m0.ascent * zoom,
            underline_pos: m0.underline_pos * zoom,
            underline_thickness: m0.underline_thickness,
            strikeout_pos: m0.strikeout_pos * zoom,
        };
        let key = |c: char, bold: bool| GlyphKey::cell_zoomed(c, bold, false, base_px, zoom);

        // ---- typing trail ------------------------------------------------
        let fade = cfg.typing_trail.fade_ms.max(50) as f32 / 1000.0;
        self.trail.retain(|g| now.duration_since(g.at).as_secs_f32() < fade);
        if !cfg.typing_trail.enabled {
            self.trail.clear();
        }
        let n = self.trail.len();
        for (i, g) in self.trail.iter().enumerate() {
            let age = now.duration_since(g.at).as_secs_f32() / fade;
            // Newest glyphs are brightest; strength also fades with age.
            let recency = (i + 1) as f32 / n as f32;
            let a = (1.0 - age).powf(1.4) * g.intensity * (0.35 + 0.65 * recency);
            if a <= 0.02 {
                continue;
            }
            let color = trail_color(&cfg.typing_trail.color, theme, age);
            if cfg.typing_trail.glow {
                let spread = 2.0 + 4.0 * (1.0 - age);
                batch.rrect(g.x - spread, g.y - spread, m.width + 2.0 * spread, m.height + 2.0 * spread, 4.0 + spread, with_alpha(color, a * 0.22));
                batch.rrect(g.x - 1.0, g.y - 1.0, m.width + 2.0, m.height + 2.0, 3.0, with_alpha(color, a * 0.35));
            }
            if let Some(gl) = fonts.glyph(key(g.ch, true)) {
                batch.glyph(g.x, g.y + m.ascent, &gl, with_alpha(color, a));
            }
            // A thin scanline under the glyph, cyberpunk style.
            batch.rect(g.x, g.y + m.height - 2.0, m.width, 1.0, with_alpha(color, a * 0.8));
        }

        // ---- paste rain --------------------------------------------------
        if let Some(p) = self.pending.as_ref()
            && now >= p.deadline {
                self.pending = None;
            }
        if !self.falls.is_empty() {
            let base = rain_color(&cfg.paste_rain.color, theme);
            let head_color = if cfg.paste_rain.color == "green" { rgb([0xd8, 0xff, 0xe0]) } else { theme.fg };
            let glyph_set = self.shimmer.clone();
            let tail = cfg.paste_rain.tail.clamp(1, 12);
            let mut landed = Vec::new();
            for f in &self.falls {
                let t = now.duration_since(f.start).as_secs_f32() - f.delay;
                if t < 0.0 {
                    continue;
                }
                let p = (t / f.dur).min(1.0);
                if p >= 1.0 {
                    landed.push((f.col, f.row));
                    continue;
                }
                // Ease in: the glyph accelerates as it falls.
                let e = p * p;
                let x = grid_x + f.col as f32 * m.width;
                let head = -1.0 + (f.row as f32 + 1.0) * e;
                let frame = (now.duration_since(f.start).as_millis() / 40) as usize + f.seed as usize;
                // Settle: show the true character for the last part of the fall.
                let settled = p > 0.72 || glyph_set.is_empty();
                for k in 0..tail {
                    let row = head - k as f32;
                    if row < -0.5 || row > f.row as f32 + 0.5 {
                        continue;
                    }
                    let y = grid_y + row.round() * m.height;
                    let fade = 1.0 - k as f32 / tail as f32;
                    let a = if k == 0 { 1.0 } else { fade * fade * 0.7 };
                    if a < 0.04 {
                        continue;
                    }
                    let ch = if (k == 0 && settled) || glyph_set.is_empty() {
                        f.ch
                    } else {
                        glyph_set[(frame + k * 7) % glyph_set.len()]
                    };
                    let color = if k == 0 { head_color } else { base };
                    if k == 0 {
                        batch.rrect(x - 1.0, y - 1.0, m.width + 2.0, m.height + 2.0, 3.0, with_alpha(base, 0.35));
                    }
                    if let Some(gl) = fonts.glyph(key(ch, k == 0)) {
                        batch.glyph(x, y + m.ascent, &gl, with_alpha(color, a));
                    }
                }
            }
            if !landed.is_empty() {
                for key in &landed {
                    self.masked.remove(key);
                }
                self.falls.retain(|f| !landed.contains(&(f.col, f.row)));
            }
        }
    }
}

fn named_color(name: &str, theme: &Theme) -> Option<Rgba> {
    match name.to_ascii_lowercase().as_str() {
        "accent" => Some(theme.accent),
        "cursor" => Some(theme.cursor),
        "green" => Some(rgb([0x39, 0xff, 0x6a])),
        "fg" => Some(theme.fg),
        s if s.starts_with('#') => Some(rgb(parse_hex(s))),
        _ => None,
    }
}

fn trail_color(name: &str, theme: &Theme, age: f32) -> Rgba {
    if name.eq_ignore_ascii_case("neon") {
        // Cursor color at the head, accent toward the tail, magenta in between.
        let a = theme.cursor;
        let b = theme.accent;
        let mag = theme.ansi[5];
        let (from, to, t) = if age < 0.5 { (a, mag, age * 2.0) } else { (mag, b, (age - 0.5) * 2.0) };
        [from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t, from[2] + (to[2] - from[2]) * t, 1.0]
    } else {
        named_color(name, theme).unwrap_or(theme.accent)
    }
}

fn rain_color(name: &str, theme: &Theme) -> Rgba {
    named_color(name, theme).unwrap_or(theme.accent)
}
