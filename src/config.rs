//! User configuration and the saved-command store.
//!
//! Two files live under `~/.config/kindlyterm/`:
//!  - `config.toml`   : font, colors, terminal settings
//!  - `commands.toml` : the list of saved, named commands

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("kindlyterm");
    // One-time migration from the app's earlier name.
    let old = base.join("kindterm");
    if !dir.exists() && old.is_dir() {
        match fs::rename(&old, &dir) {
            Ok(()) => log::info!("migrated config from {} to {}", old.display(), dir.display()),
            Err(e) => log::warn!("could not migrate {}: {e}", old.display()),
        }
    }
    dir
}

// ---------------------------------------------------------------------------
// config.toml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub font: FontConfig,
    pub terminal: TerminalConfig,
    pub colors: ColorConfig,
    pub clipboard: ClipboardConfig,
    pub input: InputConfig,
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct McpConfig {
    /// Serve the control API on a private socket so `kindlyterm --mcp`
    /// (Claude Code and other MCP clients) can read screens and type into
    /// terminals. Off by default: anything running as your user could use
    /// it while it is on.
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct InputConfig {
    /// Tapping Ctrl+Shift together (press both, release without any other
    /// key) toggles the Control Deck.
    pub ctrl_shift_tap_opens_deck: bool,
    /// Plain PageUp/PageDown scroll the history at a shell prompt (when no
    /// full-screen program or application cursor mode is active). Shift
    /// versions always scroll.
    pub page_keys_scroll: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self { ctrl_shift_tap_opens_deck: true, page_keys_scroll: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClipboardConfig {
    /// Copy to the system clipboard as soon as a mouse selection is made
    /// (the primary selection is always filled regardless).
    pub copy_on_select: bool,
    /// When text is selected, Ctrl+C copies it instead of sending an
    /// interrupt to the program. Without a selection Ctrl+C behaves normally.
    pub ctrl_c_copies_selection: bool,
    /// Ctrl+V pastes when the program has not asked for raw keys (i.e. a
    /// normal shell prompt). Full-screen apps like vim still receive Ctrl+V.
    pub ctrl_v_pastes_in_shell: bool,
    /// Strip the trailing newline from pasted text so a single copied line
    /// does not execute immediately.
    pub trim_trailing_newline: bool,
    /// When the program does not support bracketed paste (so every newline
    /// would run a command), ask before pasting multi-line text.
    pub confirm_multiline_paste: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            copy_on_select: true,
            ctrl_c_copies_selection: true,
            ctrl_v_pastes_in_shell: true,
            trim_trailing_newline: true,
            confirm_multiline_paste: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontConfig {
    /// Font family name. "monospace" resolves to the system default.
    pub family: String,
    /// Point size (logical pixels); multiplied by the window scale factor.
    pub size: f32,
    /// Extra line spacing added to the font's natural line height, in pixels.
    pub line_padding: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TerminalConfig {
    /// Lines of scrollback history kept per tab.
    pub scrollback: usize,
    /// Shell program. Defaults to $SHELL, then /bin/sh.
    pub shell: Option<String>,
    /// Arguments passed to the shell when opening a plain tab.
    pub shell_args: Vec<String>,
    /// Padding (pixels) between the window edge and the terminal grid.
    pub padding: f32,
    /// Cursor shape: "block", "beam" or "underline".
    pub cursor: String,
    /// Idle cursor behaviour: "breathe" (soft pulse), "blink", or "none".
    /// Travel and focus animations are always on unless "none".
    pub cursor_animation: String,
    /// What the cursor turns into while Backspace or Delete is held:
    /// "pacman" (default), "laser" (a cutter head with a beam and embers),
    /// or "none".
    pub chomp: String,
    /// What programs may do with the clipboard through OSC 52:
    /// "copy" (default: they can set it, never read it), "none", or "both".
    /// Reading lets any program, including a remote ssh host, exfiltrate
    /// whatever you last copied, so "both" is opt-in.
    pub osc52: String,
    /// Run each shell in a detached host process so it survives closing
    /// and reopening kindlyTerm. Closing a tab or terminal still ends it.
    pub persistent_sessions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ColorConfig {
    /// Name of the built-in theme these colors came from ("Custom" if edited).
    pub theme: String,
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    pub tab_bar: String,
    pub tab_active: String,
    pub tab_inactive_fg: String,
    pub palette_bg: String,
    pub hover: String,
    pub palette_highlight: String,
    pub accent: String,
    /// The 16 ANSI colors, in order black..white, bright black..bright white.
    pub ansi: Vec<String>,
    /// Window opacity 0.3..1.0 (terminal background and tab bar; the
    /// compositor shows what is behind the window).
    pub opacity: f32,
}


impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "monospace".into(), size: 13.0, line_padding: 0.0 }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { scrollback: 10_000, shell: None, shell_args: vec![], padding: 6.0, cursor: "block".into(), cursor_animation: "breathe".into(), chomp: "pacman".into(), osc52: "copy".into(), persistent_sessions: true }
    }
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            theme: "Kind Night".into(),
            foreground: "#d8dee9".into(),
            background: "#1b1f27".into(),
            cursor: "#e5c07b".into(),
            selection: "#3e4b5e".into(),
            tab_bar: "#12151b".into(),
            tab_active: "#1b1f27".into(),
            tab_inactive_fg: "#6b7280".into(),
            palette_bg: "#232833".into(),
            hover: "#2a3040".into(),
            palette_highlight: "#34405a".into(),
            accent: "#7aa2f7".into(),
            opacity: 1.0,
            ansi: [
                "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
                "#5c6370", "#ef7a82", "#a6d189", "#f0d197", "#74bdf7", "#d48ce6", "#6cc7d1", "#ffffff",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// Load the config, writing a default file if none exists.
    pub fn load() -> Config {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    log::error!("failed to parse {}: {e}; using defaults", path.display());
                    Config::default()
                }
            },
            Err(_) => {
                let cfg = Config::default();
                if let Err(e) = cfg.write_default(&path) {
                    log::warn!("could not write default config: {e}");
                }
                cfg
            }
        }
    }

    /// Persist the current configuration (used by the Control Deck).
    pub fn save(&self) -> Result<()> {
        self.write_default(&Self::path())
    }

    fn write_default(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(path, text)?;
        Ok(())
    }

    pub fn shell(&self) -> String {
        self.terminal
            .shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".into())
    }
}

/// Parse "#rrggbb" into an [r, g, b] triple. Falls back to magenta on error.
pub fn parse_hex(s: &str) -> [u8; 3] {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6
        && let Ok(v) = u32::from_str_radix(s, 16) {
            return [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        }
    log::warn!("bad color literal {s:?}");
    [255, 0, 255]
}

// ---------------------------------------------------------------------------
// commands.toml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedCommand {
    /// Display name, e.g. "homelab".
    pub name: String,
    /// The command line, run through the shell, e.g. "ssh dev@192.168.1.10".
    pub command: String,
    /// Optional working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// If true, drop into an interactive shell after the command exits
    /// instead of closing the tab.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_open: bool,
    /// Ask before running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm: bool,
    /// Badge glyph shown in the Deck (one character).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Badge color as an ANSI palette index 0..15.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
    /// Global hotkey, e.g. "Alt+H".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    /// Where Enter launches it: "tab" (new tab, default) or "here".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_in: Option<String>,
}

impl SavedCommand {
    pub fn new(name: String, command: String) -> Self {
        Self {
            name,
            command,
            cwd: None,
            keep_open: false,
            confirm: false,
            icon: None,
            color: None,
            hotkey: None,
            run_in: None,
        }
    }

    pub fn runs_here(&self) -> bool {
        self.run_in.as_deref() == Some("here")
    }

    /// Host part of an `ssh user@host` command, if any.
    pub fn host(&self) -> Option<String> {
        let mut words = self.command.split_whitespace();
        if words.next()? != "ssh" {
            return None;
        }
        words.find(|w| !w.starts_with('-')).map(|w| w.rsplit('@').next().unwrap_or(w).to_string())
    }
}

/// Per-shortcut usage, kept out of commands.toml so hand edits stay clean.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunStat {
    pub last_run: u64,
    pub runs: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeckState {
    #[serde(default)]
    pub recent: std::collections::HashMap<String, RunStat>,
}

impl DeckState {
    pub fn path() -> PathBuf {
        config_dir().join("state.toml")
    }

    pub fn load() -> Self {
        fs::read_to_string(Self::path()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = Self::path().parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(Self::path(), toml::to_string(self)?)?;
        Ok(())
    }

    pub fn record_run(&mut self, name: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let e = self.recent.entry(name.to_string()).or_default();
        e.last_run = now;
        e.runs += 1;
        let _ = self.save();
    }

    pub fn stat(&self, name: &str) -> RunStat {
        self.recent.get(name).cloned().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandStore {
    #[serde(default)]
    pub commands: Vec<SavedCommand>,
}

impl CommandStore {
    pub fn path() -> PathBuf {
        config_dir().join("commands.toml")
    }

    pub fn load() -> CommandStore {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::error!("failed to parse {}: {e}", path.display());
                CommandStore::default()
            }),
            Err(_) => CommandStore::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("creating config dir")?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Add or replace a command by name.
    pub fn upsert(&mut self, cmd: SavedCommand) {
        if let Some(existing) = self.commands.iter_mut().find(|c| c.name == cmd.name) {
            *existing = cmd;
        } else {
            self.commands.push(cmd);
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.commands.len();
        self.commands.retain(|c| c.name != name);
        self.commands.len() != before
    }
}
