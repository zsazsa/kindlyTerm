//! Resolved theme colors plus the built-in palettes from the Control Deck
//! design (Theme artboard).

use crate::config::{ColorConfig, parse_hex};
use crate::renderer::{Rgba, rgb};

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub fg: Rgba,
    pub bg: Rgba,
    pub cursor: Rgba,
    pub selection: Rgba,
    pub tab_bar: Rgba,
    pub tab_active: Rgba,
    pub muted: Rgba,
    pub panel: Rgba,
    pub hover: Rgba,
    pub highlight: Rgba,
    pub accent: Rgba,
    /// Ink drawn on top of colored badges.
    pub badge_ink: Rgba,
    pub ansi: [Rgba; 16],
}

impl Theme {
    pub fn from_config(c: &ColorConfig) -> Self {
        let mut ansi = [[0.0; 4]; 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            *slot = rgb(parse_hex(c.ansi.get(i).map(String::as_str).unwrap_or("#ff00ff")));
        }
        let bg = rgb(parse_hex(&c.background));
        Self {
            name: c.theme.clone(),
            fg: rgb(parse_hex(&c.foreground)),
            bg,
            cursor: rgb(parse_hex(&c.cursor)),
            selection: rgb(parse_hex(&c.selection)),
            tab_bar: rgb(parse_hex(&c.tab_bar)),
            tab_active: rgb(parse_hex(&c.tab_active)),
            muted: rgb(parse_hex(&c.tab_inactive_fg)),
            panel: rgb(parse_hex(&c.palette_bg)),
            hover: rgb(parse_hex(&c.hover)),
            highlight: rgb(parse_hex(&c.palette_highlight)),
            accent: rgb(parse_hex(&c.accent)),
            badge_ink: rgb(parse_hex(&c.tab_bar)),
            ansi,
        }
    }
}

/// A built-in palette, expressed as the same fields the config file uses so
/// applying one is just overwriting `[colors]`.
pub struct BuiltinTheme {
    pub name: &'static str,
    pub tag: &'static str,
    pub fg: &'static str,
    pub bg: &'static str,
    pub cursor: &'static str,
    pub selection: &'static str,
    pub tab_bar: &'static str,
    pub panel: &'static str,
    pub hover: &'static str,
    pub highlight: &'static str,
    pub muted: &'static str,
    pub accent: &'static str,
    pub ansi: [&'static str; 16],
}

impl BuiltinTheme {
    pub fn to_config(&self) -> ColorConfig {
        ColorConfig {
            theme: self.name.to_string(),
            foreground: self.fg.into(),
            background: self.bg.into(),
            cursor: self.cursor.into(),
            selection: self.selection.into(),
            tab_bar: self.tab_bar.into(),
            tab_active: self.bg.into(),
            tab_inactive_fg: self.muted.into(),
            palette_bg: self.panel.into(),
            hover: self.hover.into(),
            palette_highlight: self.highlight.into(),
            accent: self.accent.into(),
            opacity: 1.0,
            ansi: self.ansi.iter().map(|s| s.to_string()).collect(),
        }
    }
}

pub const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        name: "Kind Night",
        tag: "default",
        fg: "#d8dee9",
        bg: "#1b1f27",
        cursor: "#e5c07b",
        selection: "#3e4b5e",
        tab_bar: "#12151b",
        panel: "#232833",
        hover: "#2a3040",
        highlight: "#34405a",
        muted: "#6b7280",
        accent: "#7aa2f7",
        ansi: [
            "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
            "#5c6370", "#ef7a82", "#a6d189", "#f0d197", "#74bdf7", "#d48ce6", "#6cc7d1", "#ffffff",
        ],
    },
    BuiltinTheme {
        name: "Paper",
        tag: "light",
        fg: "#2b2b2b",
        bg: "#f4f2ee",
        cursor: "#2a5db0",
        selection: "#cfd8ea",
        tab_bar: "#e6e2da",
        panel: "#ebe8e2",
        hover: "#e0dcd3",
        highlight: "#d3cec3",
        muted: "#8a8a8a",
        accent: "#2a5db0",
        ansi: [
            "#2b2b2b", "#c1443b", "#3f7d3f", "#8a6d1f", "#2a5db0", "#8a4b9e", "#2f7d85", "#5c5c5c",
            "#8a8a8a", "#d9635a", "#5c9c5c", "#b08d2a", "#4a7fd4", "#a86dbd", "#4a9ca6", "#ffffff",
        ],
    },
    BuiltinTheme {
        name: "Ember",
        tag: "warm",
        fg: "#f0d0b8",
        bg: "#1a1110",
        cursor: "#ff7043",
        selection: "#3d2622",
        tab_bar: "#120b0a",
        panel: "#241816",
        hover: "#2e1e1b",
        highlight: "#3d2622",
        muted: "#7a5c4a",
        accent: "#d98a3a",
        ansi: [
            "#1a1110", "#c9603f", "#a8912f", "#e0b04a", "#b06a3a", "#a1566d", "#8a7f5a", "#f0d0b8",
            "#7a5c4a", "#ff7043", "#cbb14a", "#ffd479", "#d98a3a", "#d1798f", "#b0a678", "#fff6ee",
        ],
    },
    BuiltinTheme {
        name: "Gruvbox",
        tag: "dark",
        fg: "#ebdbb2",
        bg: "#282828",
        cursor: "#fe8019",
        selection: "#504945",
        tab_bar: "#1d2021",
        panel: "#32302f",
        hover: "#3c3836",
        highlight: "#504945",
        muted: "#928374",
        accent: "#83a598",
        ansi: [
            "#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
            "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
        ],
    },
    BuiltinTheme {
        name: "Solarized Dark",
        tag: "dark",
        fg: "#93a1a1",
        bg: "#002b36",
        cursor: "#cb4b16",
        selection: "#0f4a5a",
        tab_bar: "#00212b",
        panel: "#073642",
        hover: "#0a3d4b",
        highlight: "#0f4a5a",
        muted: "#586e75",
        accent: "#268bd2",
        ansi: [
            "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
            "#586e75", "#cb4b16", "#93a1a1", "#839496", "#657b83", "#6c71c4", "#93a1a1", "#fdf6e3",
        ],
    },
    BuiltinTheme {
        name: "Catppuccin Mocha",
        tag: "dark",
        fg: "#cdd6f4",
        bg: "#1e1e2e",
        cursor: "#f5e0dc",
        selection: "#45475a",
        tab_bar: "#11111b",
        panel: "#181825",
        hover: "#313244",
        highlight: "#45475a",
        muted: "#6c7086",
        accent: "#89b4fa",
        ansi: [
            "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
            "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
        ],
    },
];

pub fn builtin(name: &str) -> Option<&'static BuiltinTheme> {
    BUILTIN_THEMES.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

/// The 16 icon glyphs from the design's icon sheet with their badge colors
/// (ANSI index). Used by the shortcut editor's glyph picker and as defaults.
pub const ICON_SHEET: &[(char, &str, u8)] = &[
    ('⌂', "house", 4),
    ('▦', "server", 6),
    ('⛁', "database", 1),
    ('≡', "logs", 3),
    ('∿', "pulse", 2),
    ('⚒', "build", 5),
    ('☁', "cloud", 12),
    ('⚿', "lock", 9),
    ('⚷', "key", 11),
    ('❯', "prompt", 10),
    ('⚙', "gear", 7),
    ('◐', "palette", 13),
    ('⌨', "keyboard", 14),
    ('⎘', "clipboard", 8),
    ('▤', "tabs", 12),
    ('★', "star", 15),
];

/// Badge colors offered in the editor, as ANSI indices (design: 12 swatches).
pub const BADGE_COLORS: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 9, 10, 12, 13, 14];

/// Pick a sensible default icon + color for a command line.
pub fn guess_icon(name: &str, command: &str) -> (char, u8) {
    let c = command.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let has = |s: &str| c.contains(s) || n.contains(s);
    if c.starts_with("ssh") || has("ssh ") {
        if has("db") || has("postgres") || has("mysql") || has("sql") {
            ('⛁', 1)
        } else {
            ('⌂', 4)
        }
    } else if has("psql") || has("mysql") || has("sqlite") || has("mongo") || has("redis") {
        ('⛁', 1)
    } else if has("journalctl") || has("tail") || has("log") {
        ('≡', 3)
    } else if has("htop") || has("btop") || has("top") || has("watch") {
        ('∿', 2)
    } else if has("cargo") || has("make") || has("build") || has("npm") {
        ('⚒', 5)
    } else if has("aws") || has("gcloud") || has("az ") || has("kubectl") || has("k8s") {
        ('☁', 12)
    } else if has("sudo") || has("vault") || has("gpg") {
        ('⚿', 9)
    } else if has("key") || has("cert") || has("openssl") {
        ('⚷', 11)
    } else if has("git") {
        ('★', 15)
    } else {
        ('❯', 10)
    }
}
