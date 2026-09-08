//! The keyboard-driven overlay: fuzzy pickers for saved commands and tabs,
//! plus a small two-step form for saving a new command.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use crate::config::SavedCommand;
use crate::terminal::TabId;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Pick a saved command to launch.
    Commands,
    /// Pick an open tab to switch to.
    Tabs,
    /// Save a new command: first the command line, then its name.
    SaveCommand { command: Option<String> },
    /// Confirm deleting a saved command.
    ConfirmDelete { name: String },
}

/// One row shown in the list.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: ItemId,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemId {
    Command(String),
    Tab(TabId),
}

/// What the app should do after handling a key.
#[derive(Debug)]
pub enum Action {
    None,
    Close,
    LaunchCommand(String),
    SwitchTab(TabId),
    SaveCommand(SavedCommand),
    DeleteCommand(String),
}

pub struct Palette {
    pub mode: Mode,
    pub input: String,
    pub selected: usize,
    pub items: Vec<Item>,
    pub filtered: Vec<usize>,
    matcher: Matcher,
}

impl Palette {
    pub fn new(mode: Mode, items: Vec<Item>) -> Self {
        let mut p = Self {
            mode,
            input: String::new(),
            selected: 0,
            items,
            filtered: Vec::new(),
            matcher: Matcher::new(MatcherConfig::DEFAULT),
        };
        p.refilter();
        p
    }

    pub fn title(&self) -> String {
        match &self.mode {
            Mode::Commands => "Run saved command".into(),
            Mode::Tabs => "Switch tab".into(),
            Mode::SaveCommand { command: None } => "Save command  (1/2) command line".into(),
            Mode::SaveCommand { command: Some(c) } => format!("Save command  (2/2) name for: {c}"),
            Mode::ConfirmDelete { name } => format!("Delete '{name}'?  y / n"),
        }
    }

    pub fn hint(&self) -> &'static str {
        match self.mode {
            Mode::Commands => "Enter: open in new tab   Ctrl+D: delete   Esc: close",
            Mode::Tabs => "Enter: switch   Esc: close",
            Mode::SaveCommand { .. } => "Enter: next   Esc: cancel",
            Mode::ConfirmDelete { .. } => "",
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self.mode, Mode::Commands | Mode::Tabs)
    }

    fn refilter(&mut self) {
        if !self.is_list() {
            return;
        }
        if self.input.trim().is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            let pattern = Pattern::parse(&self.input, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let hay = format!("{} {}", item.label, item.detail);
                    buf.clear();
                    let hay = Utf32Str::new(&hay, &mut buf);
                    pattern.score(hay, &mut self.matcher).map(|s| (s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    pub fn insert_str(&mut self, s: &str) {
        self.input.push_str(s);
        self.refilter();
    }

    pub fn backspace(&mut self, word: bool) {
        if word {
            let trimmed = self.input.trim_end();
            let cut = trimmed.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
            self.input.truncate(cut);
        } else {
            self.input.pop();
        }
        self.refilter();
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.refilter();
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(n)) as usize;
    }

    /// Handle Enter.
    pub fn confirm(&mut self) -> Action {
        match self.mode.clone() {
            Mode::Commands => match self.selected_item().map(|i| i.id.clone()) {
                Some(ItemId::Command(name)) => Action::LaunchCommand(name),
                _ => Action::None,
            },
            Mode::Tabs => match self.selected_item().map(|i| i.id.clone()) {
                Some(ItemId::Tab(id)) => Action::SwitchTab(id),
                _ => Action::None,
            },
            Mode::SaveCommand { command: None } => {
                let cmd = self.input.trim().to_string();
                if cmd.is_empty() {
                    return Action::None;
                }
                // Suggest a name from the first word(s).
                let suggestion = suggest_name(&cmd);
                self.mode = Mode::SaveCommand { command: Some(cmd) };
                self.input = suggestion;
                Action::None
            }
            Mode::SaveCommand { command: Some(cmd) } => {
                let name = self.input.trim().to_string();
                if name.is_empty() {
                    return Action::None;
                }
                Action::SaveCommand(SavedCommand::new(name, cmd))
            }
            Mode::ConfirmDelete { .. } => Action::None,
        }
    }

    /// Ctrl+D in the command list: ask before deleting.
    pub fn request_delete(&mut self) {
        if let Some(ItemId::Command(name)) = self.selected_item().map(|i| i.id.clone()) {
            self.mode = Mode::ConfirmDelete { name };
        }
    }

    /// y/n while confirming a delete.
    pub fn confirm_delete(&mut self, yes: bool) -> Action {
        if let Mode::ConfirmDelete { name } = self.mode.clone() {
            if yes {
                return Action::DeleteCommand(name);
            }
            self.mode = Mode::Commands;
        }
        Action::None
    }
}

/// "ssh dev@192.168.1.5 -p 22" -> "dev@192.168.1.5"; "htop" -> "htop".
pub fn suggest_name(cmd: &str) -> String {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    match words.as_slice() {
        ["ssh", rest @ ..] => rest
            .iter()
            .find(|w| !w.starts_with('-'))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "ssh".into()),
        [first, ..] => first.to_string(),
        [] => String::new(),
    }
}
