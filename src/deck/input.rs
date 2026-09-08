//! Keyboard and mouse handling for the Deck.

use super::*;

impl Deck {
    // -----------------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------------

    pub(super) fn act(&mut self, act: Act, env: &DeckEnv) -> DeckAction {
        match act {
            Act::None => DeckAction::None,
            Act::Push(id) => {
                self.push(id);
                DeckAction::None
            }
            Act::Pop => self.pop(),
            Act::App(a) => match a {
                DeckAction::ApplyTheme(ref name) => {
                    self.theme_preview = false;
                    self.set_toast(format!("theme: {name}"));
                    a
                }
                DeckAction::ApplyFont(ref name) => {
                    self.font_preview = false;
                    self.set_toast(format!("font: {name}"));
                    a
                }
                DeckAction::Launch { .. } | DeckAction::RunRaw(_) => {
                    // Launch closes the Deck; the filter resets so the Deck
                    // reopens at rest.
                    self.page_mut().filter.clear();
                    let _ = self.close();
                    a
                }
                other => other,
            },
            Act::Edit(name) => {
                let existing = name.as_deref().and_then(|n| env.shortcuts.iter().find(|c| c.name == n));
                self.editor = Some(Editor::new(existing, None));
                self.push(PageId::Editor);
                self.page_mut().focus = 2;
                self.page_mut().reveal_focus = true;
                DeckAction::None
            }
            Act::EditorSave => {
                let Some(ed) = self.editor.as_mut() else { return DeckAction::None };
                ed.sync_badge();
                let mut cmd = ed.draft.clone();
                cmd.name = cmd.name.trim().to_string();
                cmd.command = cmd.command.trim().to_string();
                if cmd.name.is_empty() {
                    cmd.name = crate::palette::suggest_name(&cmd.command);
                }
                if cmd.name.is_empty() || cmd.command.is_empty() {
                    self.set_toast("name and command are required".into());
                    return DeckAction::None;
                }
                if cmd.cwd.as_deref().map(|s| s.trim().is_empty()).unwrap_or(false) {
                    cmd.cwd = None;
                }
                let previous = ed.previous.clone();
                self.editor = None;
                self.stack.pop();
                if self.stack.is_empty() {
                    self.stack.push(PageState::new(PageId::Home));
                }
                self.set_toast(format!("saved '{}'", cmd.name));
                DeckAction::SaveShortcut { cmd, previous }
            }
            Act::EditorDelete => {
                let Some(ed) = self.editor.as_ref() else { return DeckAction::None };
                let Some(name) = ed.previous.clone() else { return DeckAction::None };
                self.editor = None;
                self.stack.pop();
                if self.stack.is_empty() {
                    self.stack.push(PageState::new(PageId::Home));
                }
                self.set_toast(format!("deleted '{name}'"));
                DeckAction::DeleteShortcut(name)
            }
            Act::EditorBind => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.binding = true;
                }
                DeckAction::None
            }
            Act::EditorNext => {
                let n = self.focus_count.max(1);
                let p = self.page_mut();
                p.focus = (p.focus + 1).min(n - 1);
                p.reveal_focus = true;
                DeckAction::None
            }
            Act::EditorGlyph(d) => {
                if let Some(ed) = self.editor.as_mut() {
                    let n = ICON_SHEET.len() as i32;
                    ed.glyph_idx = ((ed.glyph_idx as i32 + d).rem_euclid(n)) as usize;
                    ed.sync_badge();
                }
                DeckAction::None
            }
            Act::EditorColor(d) => {
                if let Some(ed) = self.editor.as_mut() {
                    let n = BADGE_COLORS.len() as i32;
                    ed.color_idx = ((ed.color_idx as i32 + d).rem_euclid(n)) as usize;
                    ed.sync_badge();
                }
                DeckAction::None
            }
            Act::EditorToggle(which) => {
                if let Some(ed) = self.editor.as_mut() {
                    match which {
                        FieldId::Cwd => ed.draft.keep_open = !ed.draft.keep_open,
                        FieldId::Name => ed.draft.confirm = !ed.draft.confirm,
                        _ => {}
                    }
                }
                DeckAction::None
            }
            Act::EditorRunIn => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.draft.run_in = if ed.draft.runs_here() { None } else { Some("here".into()) };
                }
                DeckAction::None
            }
            Act::ShellCommit => {
                let v = self.shell_draft.clone().unwrap_or_default();
                let v = v.trim().to_string();
                self.shell_draft = None;
                self.set_toast(if v.is_empty() { "shell: $SHELL".into() } else { format!("shell: {v}") });
                DeckAction::SetShell(if v.is_empty() { None } else { Some(v) })
            }
        }
    }

    /// After focus moves on the Theme/Font pages, preview the focused item.
    pub(super) fn preview_for_focus(&mut self, built: &Built) -> DeckAction {
        let page = self.page();
        let f = page.focus;
        match page.id {
            PageId::Theme => {
                if let Some(Act::App(DeckAction::ApplyTheme(name))) = built.focusables.get(f).map(|x| &x.enter) {
                    self.theme_preview = true;
                    return DeckAction::PreviewTheme(Some(name.clone()));
                }
            }
            PageId::Font => {
                if let Some(Act::App(DeckAction::ApplyFont(name))) = built.focusables.get(f).map(|x| &x.enter) {
                    self.font_preview = true;
                    return DeckAction::PreviewFont(Some(name.clone()));
                }
            }
            _ => {}
        }
        DeckAction::None
    }

    pub(super) fn move_focus(&mut self, built: &Built, delta: i32, horizontal: bool) -> DeckAction {
        let n = built.focusables.len();
        if n == 0 {
            return DeckAction::None;
        }
        let cur = self.page().focus.min(n - 1);
        let grid = built.focusables[cur].grid;
        let next = match (grid, horizontal) {
            (Some((start, len, cols)), false) => {
                let i = cur - start;
                let t = i as i32 + delta * cols as i32;
                if t >= 0 && (t as usize) < len {
                    start + t as usize
                } else if delta > 0 {
                    (start + len).min(n - 1)
                } else {
                    start.saturating_sub(1)
                }
            }
            (Some((start, len, _)), true) => {
                let t = cur as i32 + delta;
                t.clamp(start as i32, (start + len - 1) as i32) as usize
            }
            (None, _) => (cur as i32 + delta).clamp(0, n as i32 - 1) as usize,
        };
        self.page_mut().focus = next;
        self.page_mut().reveal_focus = true;
        self.preview_for_focus(built)
    }

    /// Handle a key press while the Deck is open.
    pub fn key(&mut self, event: &KeyEvent, mods: ModifiersState, env: &DeckEnv) -> DeckAction {
        let built = self.build(env);
        self.focus_count = built.focusables.len();
        let ctrl = mods.control_key();
        let alt = mods.alt_key();
        let shift = mods.shift_key();
        let base = event.key_without_modifiers();

        // Hotkey capture in the editor.
        if let Some(ed) = self.editor.as_mut().filter(|e| e.binding) {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    ed.binding = false;
                }
                Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete) => {
                    ed.draft.hotkey = None;
                    ed.binding = false;
                }
                Key::Named(NamedKey::Control) | Key::Named(NamedKey::Alt) | Key::Named(NamedKey::Shift) | Key::Named(NamedKey::Super) => {}
                _ => {
                    if let Some(name) = hotkey_name(&base, mods) {
                        ed.draft.hotkey = Some(name);
                        ed.binding = false;
                    }
                }
            }
            return DeckAction::None;
        }

        // Global chords inside the Deck.
        if ctrl && !alt
            && let Key::Character(c) = &base {
                match c.to_ascii_lowercase().as_str() {
                    "s" if self.page().id == PageId::Editor => return self.act(Act::EditorSave, env),
                    "n" | "j" => return self.move_focus(&built, 1, false),
                    "p" | "k" => return self.move_focus(&built, -1, false),
                    "u" => {
                        self.edit_text(&built, TextEdit::Clear);
                        return DeckAction::None;
                    }
                    "w" => {
                        self.edit_text(&built, TextEdit::DeleteWord);
                        return DeckAction::None;
                    }
                    _ => {}
                }
            }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                if !self.page().filter.is_empty() && self.page().id != PageId::Editor {
                    self.page_mut().filter.clear();
                    self.page_mut().focus = 0;
                    self.page_mut().reveal_focus = true;
                    return DeckAction::None;
                }
                if self.stack.len() > 1 {
                    return self.pop();
                }
                return self.close();
            }
            Key::Named(NamedKey::ArrowDown) => return self.move_focus(&built, 1, false),
            Key::Named(NamedKey::ArrowUp) => return self.move_focus(&built, -1, false),
            Key::Named(NamedKey::Tab) => return self.move_focus(&built, if shift { -1 } else { 1 }, false),
            Key::Named(NamedKey::PageDown) => return self.move_focus(&built, 8, false),
            Key::Named(NamedKey::PageUp) => return self.move_focus(&built, -8, false),
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowRight) => {
                let right = matches!(event.logical_key, Key::Named(NamedKey::ArrowRight));
                let f = self.page().focus;
                let Some(fc) = built.focusables.get(f) else { return DeckAction::None };
                if fc.grid.is_some() {
                    return self.move_focus(&built, if right { 1 } else { -1 }, true);
                }
                // Text fields: arrows are ignored (no caret movement yet);
                // rows: adjust.
                if fc.field.is_some() {
                    return DeckAction::None;
                }
                let a = if right { fc.right.clone() } else { fc.left.clone() };
                if matches!(a, Act::None) && right && self.page().id == PageId::Home && fc.shortcut.is_some() {
                    // → on a shortcut opens its editor.
                    return self.act(Act::Edit(fc.shortcut.clone()), env);
                }
                return self.act(a, env);
            }
            Key::Named(NamedKey::Enter) => {
                let f = self.page().focus;
                let Some(fc) = built.focusables.get(f) else { return DeckAction::None };
                if let Some(name) = fc.shortcut.clone() {
                    if alt {
                        return self.act(Act::App(DeckAction::Launch { name, target: LaunchTarget::NewTab }), env);
                    }
                    if shift {
                        return self.act(Act::App(DeckAction::Launch { name, target: LaunchTarget::Here }), env);
                    }
                }
                let a = fc.enter.clone();
                return self.act(a, env);
            }
            Key::Named(NamedKey::Space) => {
                let f = self.page().focus;
                let fc = built.focusables.get(f);
                let is_field = fc.map(|x| x.field.is_some()).unwrap_or(false);
                let list_page = self.typing_goes_to_filter();
                if !is_field && !list_page
                    && let Some(fc) = fc {
                        let a = fc.enter.clone();
                        return self.act(a, env);
                    }
                self.edit_text(&built, TextEdit::Insert(" ".into()));
                return DeckAction::None;
            }
            Key::Named(NamedKey::Backspace) => {
                self.edit_text(&built, TextEdit::Backspace);
                return DeckAction::None;
            }
            Key::Named(NamedKey::Delete) => {
                if self.page().id == PageId::Manage {
                    // Delete on a shortcut row: open its editor at the delete step.
                    let f = self.page().focus;
                    if let Some(name) = built.focusables.get(f).and_then(|x| x.shortcut.clone()) {
                        return self.act(Act::Edit(Some(name)), env);
                    }
                }
                return DeckAction::None;
            }
            Key::Character(_) if !ctrl && !alt => {
                if let Some(text) = event.text.as_deref() {
                    self.edit_text(&built, TextEdit::Insert(text.to_string()));
                }
                return DeckAction::None;
            }
            _ => {}
        }
        DeckAction::None
    }

    /// Test hook: drive the Deck with tokens ("down", "enter", "esc", "left",
    /// "right", "tab", "space", "alt-enter") or literal text.
    pub fn debug_type(&mut self, tok: &str, env: &DeckEnv) -> DeckAction {
        let built = self.build(env);
        self.focus_count = built.focusables.len();
        match tok {
            "down" => self.move_focus(&built, 1, false),
            "up" => self.move_focus(&built, -1, false),
            "left" | "right" => {
                let right = tok == "right";
                let f = self.page().focus;
                let Some(fc) = built.focusables.get(f) else { return DeckAction::None };
                if fc.grid.is_some() {
                    return self.move_focus(&built, if right { 1 } else { -1 }, true);
                }
                let a = if right { fc.right.clone() } else { fc.left.clone() };
                self.act(a, env)
            }
            "enter" => {
                let f = self.page().focus;
                let Some(fc) = built.focusables.get(f) else { return DeckAction::None };
                let a = fc.enter.clone();
                self.act(a, env)
            }
            "alt-enter" => {
                let f = self.page().focus;
                let Some(name) = built.focusables.get(f).and_then(|x| x.shortcut.clone()) else { return DeckAction::None };
                self.act(Act::App(DeckAction::Launch { name, target: LaunchTarget::NewTab }), env)
            }
            "esc" => {
                if self.stack.len() > 1 { self.pop() } else { self.close() }
            }
            "tab" => self.move_focus(&built, 1, false),
            "ctrl-s" => self.act(Act::EditorSave, env),
            "space" => {
                self.edit_text(&built, TextEdit::Insert(" ".into()));
                DeckAction::None
            }
            "bs" => {
                self.edit_text(&built, TextEdit::Backspace);
                DeckAction::None
            }
            other => {
                self.edit_text(&built, TextEdit::Insert(other.to_string()));
                DeckAction::None
            }
        }
    }

    pub(super) fn typing_goes_to_filter(&self) -> bool {
        matches!(self.page().id, PageId::Home | PageId::Theme | PageId::Font | PageId::Keyboard | PageId::Manage)
    }

    pub(super) fn edit_text(&mut self, built: &Built, edit: TextEdit) {
        let f = self.page().focus;
        let field = built.focusables.get(f).and_then(|x| x.field);
        // Which string receives the edit?
        let target: Option<&mut String> = match field {
            Some(FieldId::Name) => self.editor.as_mut().map(|e| &mut e.draft.name),
            Some(FieldId::Command) => self.editor.as_mut().map(|e| &mut e.draft.command),
            Some(FieldId::Cwd) => self.editor.as_mut().map(|e| e.draft.cwd.get_or_insert_with(String::new)),
            Some(FieldId::Shell) => {
                if self.shell_draft.is_none() {
                    self.shell_draft = Some(String::new());
                }
                self.shell_draft.as_mut()
            }
            Some(FieldId::Hotkey) => None,
            None => {
                if self.typing_goes_to_filter() {
                    let p = self.page_mut();
                    p.focus = 0;
                    p.scroll = 0.0;
                    p.reveal_focus = true;
                    Some(&mut p.filter)
                } else {
                    None
                }
            }
        };
        let Some(s) = target else { return };
        match edit {
            TextEdit::Insert(t) => s.push_str(&t),
            TextEdit::Backspace => {
                s.pop();
            }
            TextEdit::Clear => s.clear(),
            TextEdit::DeleteWord => {
                let trimmed = s.trim_end();
                let cut = trimmed.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                s.truncate(cut);
            }
        }
        // Editing the filter resets focus to the first result so ⏎ runs
        // the top match.
        if field.is_none() {
            self.page_mut().focus = 0;
            self.page_mut().reveal_focus = true;
        }
    }

    // -----------------------------------------------------------------------
    // Mouse
    // -----------------------------------------------------------------------

    pub fn gear_hit(&self, x: f32, y: f32) -> bool {
        self.gear_rect.contains(x, y)
    }

    pub fn panel_contains(&self, x: f32, y: f32) -> bool {
        self.open && self.panel_rect.contains(x, y)
    }

    /// Returns true if the hover state changed (needs redraw).
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hit = self.rects.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i);
        let swatch = self.swatch_rects.iter().find(|(_, r)| r.contains(x, y)).map(|(n, _)| *n);
        let changed = hit != self.hover || swatch != self.hover_swatch;
        self.hover = hit;
        self.hover_swatch = swatch;
        changed
    }

    /// Left click inside the panel. Header back area pops; items activate.
    pub fn click(&mut self, x: f32, y: f32, env: &DeckEnv) -> DeckAction {
        // Footer theme swatches apply a theme directly.
        if let Some((name, _)) = self.swatch_rects.iter().find(|(_, r)| r.contains(x, y)) {
            let name = name.to_string();
            self.set_toast(format!("theme: {name}"));
            return DeckAction::ApplyTheme(name);
        }
        let built = self.build(env);
        self.focus_count = built.focusables.len();
        // Back button strip on sub-pages.
        if self.stack.len() > 1 && y >= self.panel_rect.y && y < self.panel_rect.y + HEADER_H * env.scale && x < self.panel_rect.x + 110.0 * env.scale {
            return self.pop();
        }
        let hit = self.rects.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i);
        let Some(i) = hit else { return DeckAction::None };
        let Some(fc) = built.focusables.get(i) else { return DeckAction::None };
        self.page_mut().focus = i;
        self.page_mut().reveal_focus = true;
        // Steppers: click on the right half increments, left half decrements.
        if !matches!(fc.left, Act::None) && fc.grid.is_none() && fc.field.is_none()
            && let Some((_, r)) = self.rects.iter().find(|(j, _)| *j == i) {
                let a = if x > r.x + r.w * 0.75 { fc.right.clone() } else if x > r.x + r.w * 0.55 { fc.left.clone() } else { fc.enter.clone() };
                return self.act(a, env);
            }
        if fc.field.is_some() {
            if fc.field == Some(FieldId::Hotkey) {
                return self.act(Act::EditorBind, env);
            }
            return DeckAction::None;
        }
        let a = fc.enter.clone();
        let r = self.act(a, env);
        if matches!(r, DeckAction::None) {
            return self.preview_for_focus(&built);
        }
        r
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let p = self.page_mut();
        p.scroll = (p.scroll - dy).max(0.0);
    }

}
