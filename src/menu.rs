//! A small context menu (right-click), usable with the mouse or the keyboard.

#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    NewTab,
    RunSaved,
    SaveCommand,
    CloseTab(usize),
    CloseOthers(usize),
    MoveLeft(usize),
    MoveRight(usize),
    Copy,
    Paste,
    ClearScrollback,
    RunShortcut { name: String, target: crate::deck::LaunchTarget },
    OpenDeck,
    Cancel,
    NewWindow,
    CheatSheet,
    PasteConfirmed(String),
    RenameTab(usize),
    TearOff(usize),
    MoveToWindow(usize, winit::window::WindowId),
    // --- canvas ---
    ConvertToCanvas,
    Maximize,
    NewCanvasTab,
    /// World coordinates for a new terminal's top-left.
    NewTerminalAt(f32, f32),
    FocusMode,
    FitAll,
    ResetZoom,
    /// Move terminal `TabId` to tab index.
    MoveToCanvas(crate::terminal::TabId, usize),
    CloseTerminal(crate::terminal::TabId),
    RenameItem(crate::canvas::ItemId),
    GroupSelection,
    TogglePin(crate::canvas::ItemId),
    MirrorItem(crate::canvas::ItemId),
    CloseItem(crate::canvas::ItemId),
    RenameGroup(crate::canvas::GroupId),
    ZoomGroup(crate::canvas::GroupId),
    Ungroup(crate::canvas::GroupId),
    CloseGroup(crate::canvas::GroupId),
    /// Non-interactive divider line.
    Separator,
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub shortcut: &'static str,
    pub action: MenuAction,
    pub enabled: bool,
}

impl MenuItem {
    fn new(label: &str, shortcut: &'static str, action: MenuAction) -> Self {
        Self { label: label.into(), shortcut, action, enabled: true }
    }
    fn sep() -> Self {
        Self { label: String::new(), shortcut: "", action: MenuAction::Separator, enabled: false }
    }
    fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
}

pub struct Menu {
    /// Anchor in window pixels (top-left of the menu, before clamping).
    pub x: f32,
    pub y: f32,
    pub items: Vec<MenuItem>,
    pub selected: Option<usize>,
    /// Pixel rect actually drawn last frame (x, y, w, h), used for hit tests.
    pub rect: (f32, f32, f32, f32),
    pub row_h: f32,
}

impl Menu {
    fn new(x: f32, y: f32, items: Vec<MenuItem>) -> Self {
        Self { x, y, items, selected: None, rect: (x, y, 0.0, 0.0), row_h: 0.0 }
    }

    /// Menu for a right-click on the tab bar. `tab` is the tab under the
    /// pointer, if any; `count` is the number of tabs.
    pub fn for_tab_bar(
        x: f32,
        y: f32,
        tab: Option<usize>,
        count: usize,
        has_saved: bool,
        other_windows: &[(winit::window::WindowId, String)],
        // (is single-terminal tab, item count) of the clicked tab.
        mode: Option<(bool, usize)>,
    ) -> Self {
        let mut items = vec![
            MenuItem::new("New tab", "Ctrl+Shift+T", MenuAction::NewTab),
            MenuItem::new("New canvas tab", "Ctrl+Shift+K", MenuAction::NewCanvasTab),
            MenuItem::new("New window", "Ctrl+Shift+N", MenuAction::NewWindow),
            MenuItem::new("Run a shortcut…", "Ctrl+Shift+Space", MenuAction::RunSaved).enabled(has_saved),
            MenuItem::new("Control Deck", "Ctrl+Shift+,", MenuAction::OpenDeck),
        ];
        if let Some(i) = tab {
            items.push(MenuItem::sep());
            match mode {
                Some((true, _)) => items.push(MenuItem::new("Turn into canvas", "Ctrl+Shift+Enter", MenuAction::ConvertToCanvas)),
                Some((false, n)) => items.push(MenuItem::new("Maximize terminal", "", MenuAction::Maximize).enabled(n == 1)),
                None => {}
            }
            items.push(MenuItem::new("Rename tab…", "double-click", MenuAction::RenameTab(i)));
            items.push(MenuItem::new("Move left", "", MenuAction::MoveLeft(i)).enabled(i > 0));
            items.push(MenuItem::new("Move right", "", MenuAction::MoveRight(i)).enabled(i + 1 < count));
            items.push(MenuItem::new("Move to new window", "drag out", MenuAction::TearOff(i)).enabled(count > 1));
            for (id, title) in other_windows.iter().take(6) {
                items.push(MenuItem::new(&format!("Move to window: {title}"), "", MenuAction::MoveToWindow(i, *id)));
            }
            items.push(MenuItem::sep());
            items.push(MenuItem::new("Close tab", "Ctrl+Shift+W", MenuAction::CloseTab(i)));
            items.push(MenuItem::new("Close other tabs", "", MenuAction::CloseOthers(i)).enabled(count > 1));
        }
        Self::new(x, y, items)
    }

    /// Menu for a right-click on empty free canvas.
    pub fn for_canvas(x: f32, y: f32, wx: f32, wy: f32, has_saved: bool, one_item: bool) -> Self {
        let items = vec![
            MenuItem::new("New terminal here", "Ctrl+Shift+Enter", MenuAction::NewTerminalAt(wx, wy)),
            MenuItem::new("Run a shortcut…", "Ctrl+Shift+Space", MenuAction::RunSaved).enabled(has_saved),
            MenuItem::sep(),
            MenuItem::new("Fit everything", "Ctrl+Shift+A", MenuAction::FitAll),
            MenuItem::new("Reset zoom", "Ctrl+Shift+0", MenuAction::ResetZoom),
            MenuItem::new("Group selected terminals", "Ctrl+Shift+G", MenuAction::GroupSelection),
            MenuItem::new("Maximize terminal", "", MenuAction::Maximize).enabled(one_item),
            MenuItem::sep(),
            MenuItem::new("New canvas tab", "Ctrl+Shift+K", MenuAction::NewCanvasTab),
            MenuItem::new("Control Deck", "Ctrl+Shift+,", MenuAction::OpenDeck),
            MenuItem::new("Keyboard cheat sheet", "Ctrl+/", MenuAction::CheatSheet),
        ];
        Self::new(x, y, items)
    }

    /// Menu for a right-click on a group's label.
    pub fn for_group(x: f32, y: f32, g: crate::canvas::GroupId, name: &str) -> Self {
        let items = vec![
            MenuItem::new(&format!("Zoom to {name}"), "Ctrl+Shift+F", MenuAction::ZoomGroup(g)),
            MenuItem::new("Rename group", "double-click", MenuAction::RenameGroup(g)),
            MenuItem::sep(),
            MenuItem::new("Ungroup", "Ctrl+Shift+G", MenuAction::Ungroup(g)),
            MenuItem::new("Close every terminal in it", "", MenuAction::CloseGroup(g)),
        ];
        Self::new(x, y, items)
    }

    /// Menu for a right-click on a terminal item of a free canvas.
    #[allow(clippy::too_many_arguments)]
    pub fn for_item(x: f32, y: f32, wx: f32, wy: f32, tab: crate::terminal::TabId, item: crate::canvas::ItemId, has_selection: bool, has_saved: bool, other_tabs: &[(usize, String)], pinned: bool, mirror: bool) -> Self {
        let mut items = vec![
            MenuItem::new("Copy", "Ctrl+Shift+C", MenuAction::Copy).enabled(has_selection),
            MenuItem::new("Paste", "Ctrl+Shift+V", MenuAction::Paste),
            MenuItem::sep(),
            MenuItem::new("Focus mode", "Ctrl+Shift+F", MenuAction::FocusMode),
            MenuItem::new(if pinned { "Unpin from screen" } else { "Pin to screen" }, "Ctrl+Shift+P", MenuAction::TogglePin(item)),
            MenuItem::new("Mirror here", "", MenuAction::MirrorItem(item)),
            MenuItem::new("Rename…", "double-click title", MenuAction::RenameItem(item)),
        ];
        for (ci, title) in other_tabs.iter().take(6) {
            items.push(MenuItem::new(&format!("Move to tab {}: {title}", ci + 1), "", MenuAction::MoveToCanvas(tab, *ci)));
        }
        items.extend([
            MenuItem::sep(),
            MenuItem::new("New terminal here", "Ctrl+Shift+Enter", MenuAction::NewTerminalAt(wx, wy)),
            MenuItem::new("Run a shortcut…", "Ctrl+Shift+Space", MenuAction::RunSaved).enabled(has_saved),
            MenuItem::new("Save as shortcut…", "Ctrl+Shift+S", MenuAction::SaveCommand),
            MenuItem::sep(),
            MenuItem::new("Clear scrollback", "", MenuAction::ClearScrollback),
            if mirror { MenuItem::new("Close this mirror", "Ctrl+Shift+W", MenuAction::CloseItem(item)) } else { MenuItem::new("Close terminal", "Ctrl+Shift+W", MenuAction::CloseTerminal(tab)) },
        ]);
        Self::new(x, y, items)
    }

    /// Confirmation before running a shortcut marked "confirm first".
    pub fn confirm_run(x: f32, y: f32, name: &str, target: crate::deck::LaunchTarget) -> Self {
        let mut m = Self::new(
            x,
            y,
            vec![
                MenuItem::new(&format!("Run '{name}'"), "⏎", MenuAction::RunShortcut { name: name.to_string(), target }),
                MenuItem::new("Cancel", "Esc", MenuAction::Cancel),
            ],
        );
        m.selected = Some(0);
        m
    }

    /// Confirmation before a multi-line paste into a program without
    /// bracketed paste (each newline would execute).
    pub fn confirm_paste(x: f32, y: f32, lines: usize, text: String) -> Self {
        let mut m = Self::new(
            x,
            y,
            vec![
                MenuItem::new(&format!("Paste {lines} lines (each will run)"), "⏎", MenuAction::PasteConfirmed(text)),
                MenuItem::new("Cancel", "Esc", MenuAction::Cancel),
            ],
        );
        m.selected = Some(1);
        m
    }

    /// Menu for a right-click inside the terminal area.
    pub fn for_terminal(x: f32, y: f32, tab: usize, has_selection: bool, has_saved: bool) -> Self {
        let items = vec![
            MenuItem::new("Copy", "Ctrl+Shift+C", MenuAction::Copy).enabled(has_selection),
            MenuItem::new("Paste", "Ctrl+Shift+V", MenuAction::Paste),
            MenuItem::sep(),
            MenuItem::new("New tab", "Ctrl+Shift+T", MenuAction::NewTab),
            MenuItem::new("Run a shortcut…", "Ctrl+Shift+Space", MenuAction::RunSaved).enabled(has_saved),
            MenuItem::new("Save as shortcut…", "Ctrl+Shift+S", MenuAction::SaveCommand),
            MenuItem::new("Turn into canvas", "Ctrl+Shift+Enter", MenuAction::ConvertToCanvas),
            MenuItem::new("Control Deck", "Ctrl+Shift+,", MenuAction::OpenDeck),
            MenuItem::new("Keyboard cheat sheet", "Ctrl+/", MenuAction::CheatSheet),
            MenuItem::sep(),
            MenuItem::new("Clear scrollback", "", MenuAction::ClearScrollback),
            MenuItem::new("Close tab", "Ctrl+Shift+W", MenuAction::CloseTab(tab)),
        ];
        Self::new(x, y, items)
    }

    fn selectable(&self, i: usize) -> bool {
        self.items.get(i).map(|it| it.enabled && it.action != MenuAction::Separator).unwrap_or(false)
    }

    pub fn move_selection(&mut self, delta: i32) {
        let n = self.items.len() as i32;
        if n == 0 {
            return;
        }
        let mut i = match self.selected {
            Some(s) => s as i32,
            None if delta > 0 => -1,
            None => n,
        };
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            if self.selectable(i as usize) {
                self.selected = Some(i as usize);
                return;
            }
        }
    }

    /// Index of the item at window pixel (px, py), if it is selectable.
    pub fn hit(&self, px: f32, py: f32) -> Option<usize> {
        let (x, y, w, h) = self.rect;
        if px < x || px >= x + w || py < y || py >= y + h || self.row_h <= 0.0 {
            return None;
        }
        let i = ((py - y - self.row_h * 0.25) / self.row_h).floor();
        if i < 0.0 {
            return None;
        }
        let i = i as usize;
        self.selectable(i).then_some(i)
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        let (x, y, w, h) = self.rect;
        px >= x && px < x + w && py >= y && py < y + h
    }

    pub fn selected_action(&self) -> Option<MenuAction> {
        self.selected.filter(|&i| self.selectable(i)).map(|i| self.items[i].action.clone())
    }

    pub fn width_cells(&self) -> usize {
        self.items
            .iter()
            .map(|it| it.label.chars().count() + if it.shortcut.is_empty() { 0 } else { it.shortcut.len() + 3 })
            .max()
            .unwrap_or(10)
            + 4
    }
}
