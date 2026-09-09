//! Keyboard and mouse input for a window.

use super::*;

impl App {
    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    pub(super) fn on_key(&mut self, event: KeyEvent, event_loop: &ActiveEventLoop) {
        // Space held + left-drag pans a free canvas (the key still types).
        if matches!(event.logical_key, Key::Named(NamedKey::Space)) && !self.wins.is_empty() {
            self.win_mut().space_held = event.state == ElementState::Pressed;
        }
        // Pac-Man mode: track how long Backspace/Delete has been held.
        let is_eater = matches!(event.logical_key, Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::Delete));
        if let Some(view) = self.wins.get_mut(self.cur).and_then(|w| w.view_mut()) {
            let now = Instant::now();
            let anim = &mut view.cursor_anim;
            match (event.state, is_eater) {
                (ElementState::Pressed, true) => {
                    let left = matches!(event.logical_key, Key::Named(NamedKey::Backspace));
                    match anim.chomp {
                        Some((start, _, l)) if l == left && event.repeat => anim.chomp = Some((start, now, left)),
                        _ => anim.chomp = Some((now, now, left)),
                    }
                }
                (ElementState::Released, true) => {
                    // Let the last chomp finish, then revert.
                    if let Some((start, _, left)) = anim.chomp {
                        anim.chomp = if start.elapsed().as_millis() >= CursorAnim::CHOMP_AFTER_MS { Some((start, now, left)) } else { None };
                    }
                }
                (ElementState::Pressed, false) => anim.chomp = None,
                _ => {}
            }
        }
        if event.state != ElementState::Pressed {
            // Key releases matter only to applications using the kitty
            // keyboard protocol with event reporting.
            let wants_release = self.win().active_term().map(|t| t.term.lock().mode().contains(TermMode::REPORT_EVENT_TYPES)).unwrap_or(false);
            if wants_release && self.win().rename.is_none() && self.win().menu.is_none() && !self.wins[self.cur].deck.is_open() {
                let mode = *self.win().active_term().expect("term").term.lock().mode();
                if let Some(bytes) = keys::encode(&event, self.mods, mode)
                    && let Some(tab) = self.win().active_term()
                {
                    tab.write(bytes);
                }
            }
            return;
        }
        // A real key while Ctrl+Shift are held means a chord, not a tap.
        if !matches!(event.logical_key, Key::Named(NamedKey::Control) | Key::Named(NamedKey::Shift) | Key::Named(NamedKey::Alt) | Key::Named(NamedKey::Super)) {
            self.chord_armed = None;
        }
        if self.win().cheat {
            // Any key dismisses the cheat sheet.
            self.win_mut().cheat = false;
            self.request_redraw();
            return;
        }
        if self.win().rename.is_some() {
            self.rename_key(&event);
            return;
        }
        if self.wins[self.cur].menu.is_some() {
            self.menu_key(&event, event_loop);
            return;
        }
        if self.wins[self.cur].deck.is_open() {
            // Ctrl+Shift+, toggles even while open.
            let base = event.key_without_modifiers();
            if self.mods.control_key() && self.mods.shift_key() && matches!(&base, Key::Character(c) if c == ",") {
                let a = self.wins[self.cur].deck.close();
                self.apply_deck_action(a);
                return;
            }
            let action = {
                let fam = self.win().fonts.family.clone();
                let env = deck_env!(self, fam);
                self.wins[self.cur].deck.key(&event, self.mods, &env)
            };
            self.apply_deck_action(action);
            self.request_redraw();
            return;
        }
        if self.wins[self.cur].palette.is_some() {
            self.palette_key(&event);
            return;
        }
        // Shortcut hotkeys (e.g. Alt+H) bound in the Deck editor.
        {
            let base = event.key_without_modifiers();
            if let Some(name) = self
                .store
                .commands
                .iter()
                .find(|c| c.hotkey.as_deref().map(|h| hotkey_matches(h, &base, self.mods)).unwrap_or(false))
                .map(|c| c.name.clone())
            {
                self.apply_deck_action(DeckAction::Launch { name, target: LaunchTarget::Saved });
                return;
            }
        }
        if self.global_shortcut(&event, event_loop) {
            self.request_redraw();
            return;
        }
        if let Some(v) = self.win_mut().view_mut() {
            v.cursor_anim.last_input = Instant::now();
        }
        let mode = match self.win().active_term() {
            Some(tab) => *tab.term.lock().mode(),
            None => return,
        };
        if let Some(bytes) = keys::encode(&event, self.mods, mode) {
            // Typing trail: remember the glyph at the cell it will land in.
            if !self.mods.control_key() && !self.mods.alt_key()
                && let (Key::Character(_), Some(text)) = (&event.logical_key, event.text.as_deref()) {
                    let cfg = self.effects.typing_trail.clone();
                    if let Some(v) = self.win_mut().view_mut() {
                        let (col, row) = v.cursor_anim.to;
                        for (i, ch) in text.chars().enumerate() {
                            v.fx.typed(&cfg, ch, col + i as f32, row);
                        }
                    }
                }
            let Some(tab) = self.win().active_term() else { return };
            // Typing jumps back to the live view.
            {
                let mut term = tab.term.lock();
                if term.grid().display_offset() != 0 {
                    term.scroll_display(Scroll::Bottom);
                }
                term.selection = None;
            }
            tab.write(bytes);
            self.request_redraw();
        }
    }

    /// Is the focused terminal at a plain prompt, as far as we can tell:
    /// primary screen, no application cursor mode, no kitty protocol?
    fn prompt_scroll_ok(&self) -> bool {
        self.win()
            .active_term()
            .map(|t| {
                let m = *t.term.lock().mode();
                !m.contains(TermMode::ALT_SCREEN) && !m.contains(TermMode::APP_CURSOR) && !m.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
            })
            .unwrap_or(false)
    }

    /// Returns true if the key was consumed as an app-level shortcut.
    pub(super) fn global_shortcut(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
        let m = self.mods;
        let ctrl = m.control_key();
        let shift = m.shift_key();
        let alt = m.alt_key();
        let base = event.key_without_modifiers();

        // Ctrl+Shift+<letter> chords.
        if ctrl && shift && !alt
            && let Key::Character(c) = &base {
                match c.to_ascii_lowercase().as_str() {
                    "t" => {
                        let l = self.shell_launch();
                        self.open_tab(l);
                        return true;
                    }
                    "w" => {
                        if self.on_free_canvas() {
                            if let Some(f) = self.win().canvas().and_then(|c| c.focus) {
                                self.close_item(f, event_loop);
                            } else {
                                self.close_tab(self.wins[self.cur].active, event_loop);
                            }
                        } else {
                            self.close_tab(self.wins[self.cur].active, event_loop);
                        }
                        return true;
                    }
                    "n" => {
                        self.create_window(event_loop, super::windows::NewWindow::Shell);
                        return true;
                    }
                    "p" => {
                        // On a canvas: pin the focused terminal. Elsewhere the
                        // old quick-run binding still works (Ctrl+Shift+Space
                        // is the primary one now).
                        if self.on_free_canvas()
                            && let Some(f) = self.win().canvas().and_then(|c| c.focus)
                        {
                            self.toggle_pin(f);
                        } else {
                            self.wins[self.cur].deck.open(PageId::Home);
                        }
                        return true;
                    }
                    "," | "<" => {
                        let a = self.wins[self.cur].deck.toggle();
                        self.apply_deck_action(a);
                        return true;
                    }
                    "o" => {
                        self.open_palette(Mode::Tabs);
                        return true;
                    }
                    "s" => {
                        self.open_shortcut_editor();
                        return true;
                    }
                    "c" => {
                        self.copy_selection();
                        return true;
                    }
                    "v" => {
                        self.paste();
                        return true;
                    }
                    "q" => {
                        event_loop.exit();
                        return true;
                    }
                    "k" => {
                        self.open_canvas_tab();
                        return true;
                    }
                    "f" => {
                        if self.on_free_canvas() {
                            self.toggle_focus_mode();
                        } else {
                            self.set_status("Focus mode works on a canvas tab (Ctrl+Shift+Enter turns this one into a canvas)".into());
                        }
                        return true;
                    }
                    "a" => {
                        if self.on_free_canvas() {
                            self.fit_all();
                        }
                        return true;
                    }
                    "g" => {
                        self.toggle_group();
                        return true;
                    }
                    "=" | "+" => {
                        if self.on_free_canvas() { self.zoom_by(1.25, None) } else { self.set_font_pt(self.font_pt + 1.0) }
                        return true;
                    }
                    "-" | "_" => {
                        if self.on_free_canvas() { self.zoom_by(0.8, None) } else { self.set_font_pt(self.font_pt - 1.0) }
                        return true;
                    }
                    "0" | ")" => {
                        if self.on_free_canvas() { self.reset_zoom() } else { self.set_font_pt(self.config.font.size) }
                        return true;
                    }
                    _ => {}
                }
            }
        // Ctrl+Shift+Enter: add a terminal to the canvas (converting a
        // single tab); Ctrl+Shift+Space: Deck quick-run; Ctrl+Shift+W on a
        // canvas closes only the focused terminal.
        if ctrl && shift && !alt {
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => {
                    let l = self.shell_launch();
                    self.new_terminal_in_canvas(l, None, None);
                    return true;
                }
                Key::Named(NamedKey::Space) => {
                    self.wins[self.cur].deck.open(PageId::Home);
                    return true;
                }
                _ => {}
            }
        }
        // Zoom without shift too (Ctrl+= / Ctrl+-), common muscle memory.
        if ctrl && !shift && !alt
            && let Key::Character(c) = &base {
                match c.as_str() {
                    "=" | "+" => {
                        self.set_font_pt(self.font_pt + 1.0);
                        return true;
                    }
                    "-" => {
                        self.set_font_pt(self.font_pt - 1.0);
                        return true;
                    }
                    "0" => {
                        self.set_font_pt(self.config.font.size);
                        return true;
                    }
                    _ => {}
                }
            }
        // Ctrl+/ shows the keyboard cheat sheet (Ctrl+_ still reaches readline).
        if ctrl && !alt
            && let Key::Character(c) = &base
                && c == "/" {
                    let w = self.win_mut();
                    w.cheat = !w.cheat;
                    return true;
                }
        // Plain Ctrl+C with a selection copies instead of interrupting.
        // Plain Ctrl+V pastes at a normal prompt (not inside full-screen apps).
        if ctrl && !shift && !alt
            && let Key::Character(c) = &base {
                match c.as_str() {
                    "c" if self.config.clipboard.ctrl_c_copies_selection && self.has_selection() => {
                        if self.copy_selection()
                            && let Some(tab) = self.win().active_term() {
                                tab.term.lock().selection = None;
                            }
                        return true;
                    }
                    "v" if self.config.clipboard.ctrl_v_pastes_in_shell && !self.app_wants_raw_keys() => {
                        self.paste();
                        return true;
                    }
                    _ => {}
                }
            }
        // Classic Linux keys: Ctrl+Insert copies, Shift+Insert pastes.
        if let Key::Named(NamedKey::Insert) = &event.logical_key {
            if ctrl && !shift {
                self.copy_selection();
                return true;
            }
            if shift && !ctrl {
                self.paste();
                return true;
            }
        }
        // Alt+1..9: jump to tab.
        if alt && !ctrl
            && let Key::Character(c) = &base
                && let Some(d) = c.chars().next().and_then(|ch| ch.to_digit(10))
                    && d >= 1 {
                        let idx = if d == 9 { self.wins[self.cur].canvases.len().saturating_sub(1) } else { d as usize - 1 };
                        self.switch_tab(idx);
                        return true;
                    }
        if let Key::Named(named) = &event.logical_key {
            match named {
                NamedKey::Tab if ctrl => {
                    if self.on_free_canvas() && self.win().canvas().map(|c| c.items.len() > 1).unwrap_or(false) {
                        // Cycle focus through the canvas' terminals.
                        let w = self.win();
                        let c = w.canvas().expect("canvas");
                        let ids: Vec<ItemId> = c.items.iter().map(|i| i.id).collect();
                        let cur = c.focus.and_then(|f| ids.iter().position(|&i| i == f)).unwrap_or(0);
                        let n = ids.len();
                        let next = if shift { (cur + n - 1) % n } else { (cur + 1) % n };
                        self.focus_item(ids[next]);
                        return true;
                    }
                    let n = self.wins[self.cur].canvases.len();
                    if n > 0 {
                        let next = if shift { (self.wins[self.cur].active + n - 1) % n } else { (self.wins[self.cur].active + 1) % n };
                        self.switch_tab(next);
                    }
                    return true;
                }
                NamedKey::PageDown if ctrl && !shift => {
                    let n = self.wins[self.cur].canvases.len();
                    if n > 0 {
                        self.switch_tab((self.wins[self.cur].active + 1) % n);
                    }
                    return true;
                }
                NamedKey::PageUp if ctrl && !shift => {
                    let n = self.wins[self.cur].canvases.len();
                    if n > 0 {
                        self.switch_tab((self.wins[self.cur].active + n - 1) % n);
                    }
                    return true;
                }
                NamedKey::PageUp if shift && !ctrl => {
                    self.scroll_active(Scroll::PageUp);
                    return true;
                }
                NamedKey::PageDown if shift && !ctrl => {
                    self.scroll_active(Scroll::PageDown);
                    return true;
                }
                NamedKey::PageUp | NamedKey::PageDown if !shift && !ctrl && !alt && self.config.input.page_keys_scroll && self.prompt_scroll_ok() => {
                    // At a plain prompt nothing reads PageUp, so it scrolls
                    // history; full-screen programs still get the key.
                    self.scroll_active(if matches!(named, NamedKey::PageUp) { Scroll::PageUp } else { Scroll::PageDown });
                    return true;
                }
                NamedKey::ArrowUp if shift && ctrl => {
                    self.scroll_active(Scroll::Delta(1));
                    return true;
                }
                NamedKey::ArrowDown if shift && ctrl => {
                    self.scroll_active(Scroll::Delta(-1));
                    return true;
                }
                NamedKey::Home if shift && !ctrl => {
                    self.scroll_active(Scroll::Top);
                    return true;
                }
                NamedKey::End if shift && !ctrl => {
                    self.scroll_active(Scroll::Bottom);
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// True when the foreground program is a full-screen app (alternate
    /// screen) or has enabled the kitty keyboard protocol, both of which
    /// signal it wants every key for itself.
    pub(super) fn app_wants_raw_keys(&self) -> bool {
        self.active_tab()
            .map(|t| {
                let m = *t.term.lock().mode();
                m.contains(TermMode::ALT_SCREEN) || m.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
            })
            .unwrap_or(false)
    }

    pub(super) fn scroll_active(&mut self, scroll: Scroll) {
        if let Some(tab) = self.win().active_term() {
            let alt_screen = tab.term.lock().mode().contains(TermMode::ALT_SCREEN);
            if !alt_screen {
                tab.scroll(scroll);
                self.request_redraw();
            }
        }
    }

    /// Copy the current selection to the system clipboard. Returns true if
    /// something was copied.
    pub(super) fn copy_selection(&mut self) -> bool {
        let Some(tab) = self.win().active_term() else { return false };
        let text = tab.term.lock().selection_to_string();
        let Some(text) = text.filter(|t| !t.is_empty()) else { return false };
        let n = text.chars().count();
        let lines = text.lines().count();
        match self.clipboard.as_mut().map(|cb| cb.set_text(text)) {
            Some(Ok(())) => {
                let what = if lines > 1 { format!("{lines} lines") } else { format!("{n} chars") };
                self.set_status(format!("copied {what}"));
                true
            }
            Some(Err(e)) => {
                self.set_status(format!("copy failed: {e}"));
                false
            }
            None => {
                self.set_status("copy failed: no clipboard".into());
                false
            }
        }
    }

    pub(super) fn has_selection(&self) -> bool {
        self.active_tab()
            .map(|t| t.term.lock().selection.as_ref().map(|s| !s.is_empty()).unwrap_or(false))
            .unwrap_or(false)
    }

    pub(super) fn paste(&mut self) {
        let text = self.clipboard.as_mut().and_then(|c| c.get_text().ok()).filter(|t| !t.is_empty());
        match text {
            Some(text) => self.paste_text(text),
            None => {
                // No text: a copied picture lands on the canvas.
                if !self.paste_image() {
                    self.set_status("clipboard is empty".into());
                }
            }
        }
    }

    pub(super) fn paste_text(&mut self, text: String) {
        if self.win().active_term().is_none() {
            return;
        }
        let mut text = Self::sanitize_paste(&text);
        // Without bracketed paste every newline runs a command: ask first.
        let lines = text.lines().count();
        if lines > 1 && self.config.clipboard.confirm_multiline_paste {
            let bracketed = self.win().active_term().map(|t| t.term.lock().mode().contains(TermMode::BRACKETED_PASTE)).unwrap_or(false);
            if !bracketed && self.win().menu.is_none() {
                let (w, h) = (self.win().renderer.width as f32, self.win().renderer.height as f32);
                self.win_mut().menu = Some(Menu::confirm_paste(w / 2.0 - 140.0, h / 2.0 - 40.0, lines, text));
                self.request_redraw();
                return;
            }
        }
        if self.config.clipboard.trim_trailing_newline && lines == 1 {
            while text.ends_with('\n') || text.ends_with('\r') {
                text.pop();
            }
        }
        if text.is_empty() {
            return;
        }
        self.arm_paste_rain(&text);
        let w = self.win();
        let Some(tab) = w.active_term() else { return };
        let bracketed = tab.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            tab.write(bytes);
        } else {
            let text = text.replace("\r\n", "\r").replace('\n', "\r");
            tab.write(text.into_bytes());
        }
        tab.scroll(Scroll::Bottom);
        self.request_redraw();
    }

    /// Paste rain: capture the screen before the shell echoes the paste, so
    /// the cells it changes can be dropped in from the top.
    pub(super) fn arm_paste_rain(&mut self, text: &str) {
        let cfg = self.effects.paste_rain.clone();
        if let Some(tab) = self.win_mut().active_term_mut() {
            Self::arm_paste_rain_on(tab, &cfg, text);
        }
    }

    /// Same, for a given terminal (tool input may land on any of them).
    pub(super) fn arm_paste_rain_on(tab: &mut crate::terminal::Terminal, cfg: &crate::effects::RainConfig, text: &str) {
        if !cfg.enabled || text.chars().count() < cfg.min_chars {
            return;
        }
        let snap = {
            let mut term = tab.term.lock();
            term.scroll_display(Scroll::Bottom);
            let grid = term.grid();
            let (cols, rows) = (grid.columns(), grid.screen_lines());
            let mut cells = vec![' '; cols * rows];
            for c in grid.display_iter() {
                if let Some(vp) = point_to_viewport(grid.display_offset(), c.point)
                    && vp.line < rows && vp.column.0 < cols {
                        cells[vp.line * cols + vp.column.0] = c.c;
                    }
            }
            GridSnapshot { cells, cols, rows, history: grid.history_size() }
        };
        tab.view.fx.begin_paste(cfg, text, snap);
    }

    /// Remove anything from pasted text that could act as a command to the
    /// terminal or the program rather than as text: C0/C1 control characters
    /// (except tab, newline, carriage return) and the bracketed-paste end
    /// marker, which would let a crafted paste break out of the bracket.
    pub(super) fn sanitize_paste(text: &str) -> String {
        let text = text.replace("\x1b[201~", "");
        text.chars()
            .filter(|&c| c == '\t' || c == '\n' || c == '\r' || !(c.is_control() || ('\u{80}'..='\u{9f}').contains(&c)))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Mouse
    // -----------------------------------------------------------------------

    /// Grid point + side under the mouse cursor, if inside the grid.
    pub(super) fn mouse_point(&self) -> Option<(Point, Side)> {
        let l = self.wins[self.cur].layout?;
        let tab = self.active_tab()?;
        let (gx, gy, zoom) = self.focused_grid_origin()?;
        let (cw, ch) = (l.cell_w * zoom, l.cell_h * zoom);
        let x = (self.wins[self.cur].mouse.x as f32 - gx).max(0.0);
        let y = (self.wins[self.cur].mouse.y as f32 - gy).max(0.0);
        let col = ((x / cw) as usize).min(tab.size.cols.saturating_sub(1));
        let line = ((y / ch) as usize).min(tab.size.rows.saturating_sub(1));
        let frac = (x / cw) - col as f32;
        let side = if frac < 0.5 { Side::Left } else { Side::Right };
        let display_offset = tab.term.lock().grid().display_offset();
        Some((viewport_to_point(display_offset, Point::new(line, Column(col))), side))
    }

    pub(super) fn tab_at(&self, x: f32) -> Option<TabHit> {
        self.wins[self.cur].tab_hits.iter().copied().find(|h| x >= h.x0 && x < h.x1)
    }

    pub(super) fn in_tab_bar(&self, y: f32) -> bool {
        self.wins[self.cur].layout.map(|l| y < l.tab_bar_h).unwrap_or(false)
    }

    pub(super) fn hover_at(&self, x: f32, y: f32) -> Hover {
        if !self.in_tab_bar(y) {
            return Hover::None;
        }
        if let Some(h) = self.tab_at(x) {
            if x >= h.close_x0 && x < h.close_x1 {
                return Hover::Close(h.index);
            }
            return Hover::Tab(h.index);
        }
        if let Some((x0, x1)) = self.wins[self.cur].plus_hit
            && x >= x0 && x < x1 {
                return Hover::Plus;
            }
        Hover::None
    }

    pub(super) fn on_mouse_button(&mut self, state: ElementState, button: MouseButton, event_loop: &ActiveEventLoop) {
        let (mx, my) = (self.wins[self.cur].mouse.x as f32, self.wins[self.cur].mouse.y as f32);
        if state == ElementState::Pressed {
            self.chord_armed = None;
        }

        // An open menu swallows the click: either pick an item or dismiss.
        if let Some(menu) = self.wins[self.cur].menu.as_ref() {
            if state == ElementState::Pressed {
                if let Some(i) = menu.hit(mx, my) {
                    let action = menu.items[i].action.clone();
                    if button == MouseButton::Left {
                        self.run_menu_action(action, event_loop);
                    }
                } else if !menu.contains(mx, my) {
                    self.wins[self.cur].menu = None;
                    self.request_redraw();
                    // A right-click elsewhere opens a new menu there.
                    if button == MouseButton::Right {
                        self.on_mouse_button(state, button, event_loop);
                    }
                }
            }
            return;
        }
        if self.wins[self.cur].palette.is_some() {
            if state == ElementState::Pressed && button == MouseButton::Right {
                self.wins[self.cur].palette = None;
                self.request_redraw();
            }
            return;
        }

        // Gear button toggles the Deck.
        if state == ElementState::Pressed && button == MouseButton::Left && self.wins[self.cur].deck.gear_hit(mx, my) {
            let a = self.wins[self.cur].deck.toggle();
            self.apply_deck_action(a);
            return;
        }
        // Deck open: clicks inside the panel go to it, outside close it.
        if self.wins[self.cur].deck.is_open() {
            if state != ElementState::Pressed {
                return;
            }
            if self.wins[self.cur].deck.panel_contains(mx, my) {
                if button == MouseButton::Left {
                    let action = {
                        let fam = self.win().fonts.family.clone();
                let env = deck_env!(self, fam);
                        self.wins[self.cur].deck.click(mx, my, &env)
                    };
                    self.apply_deck_action(action);
                }
            } else if !self.in_tab_bar(my) || button == MouseButton::Left {
                let a = self.wins[self.cur].deck.close();
                self.apply_deck_action(a);
            }
            self.request_redraw();
            return;
        }
        if self.win().cheat {
            if state == ElementState::Pressed {
                self.win_mut().cheat = false;
                self.request_redraw();
            }
            return;
        }
        // A click anywhere else commits an inline rename.
        if state == ElementState::Pressed
            && let Some((ri, _, _)) = self.win().rename.as_ref().map(|(i, t, a)| (*i, t.clone(), *a)) {
                let on_same = self.in_tab_bar(my) && self.tab_at(mx).map(|h| h.index == ri).unwrap_or(false);
                if !on_same {
                    self.end_rename(true);
                } else if button == MouseButton::Left {
                    return;
                }
            }

        // End of a tab drag (the pointer may be anywhere by now).
        if button == MouseButton::Left && state == ElementState::Released
            && let Some(d) = self.win_mut().drag.take()
                && d.active {
                    if d.outside
                        && let Some(cid) = self.win().canvases.get(d.index).map(|c| c.id) {
                            self.start_pending_drop(cid);
                        }
                    self.request_redraw();
                    return;
                }

        // Tab bar.
        if self.in_tab_bar(my) {
            if state != ElementState::Pressed {
                return;
            }
            log::debug!("tab bar press at ({mx:.0},{my:.0}) -> {:?}, hits {:?}", self.hover_at(mx, my), self.win().tab_hits.iter().map(|h| (h.x0, h.x1)).collect::<Vec<_>>());
            match (button, self.hover_at(mx, my)) {
                (MouseButton::Left, Hover::Close(i)) | (MouseButton::Middle, Hover::Close(i)) => self.close_tab(i, event_loop),
                (MouseButton::Left, Hover::Tab(i)) => {
                    // Double-click on the title starts an inline rename.
                    let now = Instant::now();
                    let dbl = matches!(self.win().last_tab_click, Some((t, j)) if j == i && now.duration_since(t).as_millis() < 400);
                    self.win_mut().last_tab_click = Some((now, i));
                    if dbl {
                        self.win_mut().last_tab_click = None;
                        self.switch_tab(i);
                        self.start_rename(i);
                        return;
                    }
                    self.switch_tab(i);
                    let grab_dx = self.tab_at(mx).map(|h| mx - h.x0).unwrap_or(0.0);
                    log::debug!("tab drag start: tab {i} grab_dx {grab_dx:.0}");
                    self.win_mut().drag = Some(TabDrag { index: i, grab_dx, press_x: mx, press_y: my, active: false, outside: false });
                }
                (MouseButton::Middle, Hover::Tab(i)) => self.close_tab(i, event_loop),
                (MouseButton::Left, Hover::Plus) => {
                    let l = self.shell_launch();
                    self.open_tab(l);
                }
                (MouseButton::Right, _) => self.open_tab_bar_menu(mx, my),
                _ => {}
            }
            return;
        }

        // Free canvas: items, panning, resizing.
        if (self.on_free_canvas() || matches!(self.win().cdrag, CDrag::Pan { .. }))
            && self.canvas_mouse_button(state, button, event_loop) {
                return;
            }

        // Terminal area (single-terminal tab).
        match button {
            MouseButton::Right => {
                if state == ElementState::Pressed {
                    self.open_terminal_menu(mx, my);
                }
            }
            MouseButton::Middle => {
                if state == ElementState::Pressed {
                    self.paste_primary();
                }
            }
            MouseButton::Left => {
                if state == ElementState::Pressed
                    && self.mods.control_key()
                    && let Some(hit) = self.link_under_pointer()
                {
                    self.open_link(&hit.uri);
                    return;
                }
                self.on_left_button(state)
            }
            _ => {}
        }
    }

    pub(super) fn on_left_button(&mut self, state: ElementState) {
        match state {
            ElementState::Pressed => {
                if let Some(v) = self.win_mut().view_mut() {
                    v.cursor_anim.pulse_start = Some(Instant::now());
                }
                let Some((point, side)) = self.mouse_point() else { return };
                // Click counting for word/line selection.
                let now = Instant::now();
                self.wins[self.cur].click_count = match self.wins[self.cur].last_click {
                    Some((t, p)) if now.duration_since(t).as_millis() < 400 && p == point => (self.wins[self.cur].click_count % 3) + 1,
                    _ => 1,
                };
                self.wins[self.cur].last_click = Some((now, point));
                let ty = match self.wins[self.cur].click_count {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                if let Some(tab) = self.win().active_term() {
                    let mut term = tab.term.lock();
                    if self.mods.shift_key() && term.selection.is_some() {
                        if let Some(sel) = term.selection.as_mut() {
                            sel.update(point, side);
                        }
                    } else {
                        term.selection = Some(Selection::new(ty, point, side));
                    }
                }
                self.wins[self.cur].selecting = true;
                self.request_redraw();
            }
            ElementState::Released => {
                self.wins[self.cur].selecting = false;
                let mut text = None;
                if let Some(tab) = self.win().active_term() {
                    let mut term = tab.term.lock();
                    if term.selection.as_ref().map(|s| s.is_empty()).unwrap_or(false) {
                        term.selection = None;
                    } else {
                        text = term.selection_to_string();
                    }
                }
                // Selecting text always fills the primary selection (middle
                // click), and optionally the clipboard too.
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    if let Some(cb) = self.clipboard.as_mut() {
                        let _ = cb.set().clipboard(LinuxClipboardKind::Primary).text(text.clone());
                    }
                    if self.config.clipboard.copy_on_select {
                        self.copy_selection();
                    }
                }
                self.request_redraw();
            }
        }
    }

    pub(super) fn paste_primary(&mut self) {
        let text = self.clipboard.as_mut().and_then(|c| c.get().clipboard(LinuxClipboardKind::Primary).text().ok());
        if let Some(text) = text {
            self.paste_text(text);
        }
    }

    pub(super) fn on_mouse_move(&mut self) {
        let (mx, my) = (self.wins[self.cur].mouse.x as f32, self.wins[self.cur].mouse.y as f32);

        if let Some(menu) = self.wins[self.cur].menu.as_mut() {
            let hit = menu.hit(mx, my);
            if hit.is_some() && hit != menu.selected {
                menu.selected = hit;
                self.request_redraw();
            }
            self.set_cursor(CursorIcon::Default);
            return;
        }

        // Free canvas drags and hover.
        if self.win().drag.is_none() && (self.on_free_canvas() || matches!(self.win().cdrag, CDrag::Pan { .. })) && self.canvas_mouse_move() {
            return;
        }

        // Tab drag: reorder while inside the bar, flag "outside" past it.
        if let Some(mut d) = self.win().drag {
            let l = self.win().layout;
            if !d.active && (mx - d.press_x).abs().max((my - d.press_y).abs()) > 4.0 {
                d.active = true;
            }
            if d.active {
                let (width, height, bar_h) = {
                    let w = self.win();
                    (w.renderer.width as f32, w.renderer.height as f32, l.map(|l| l.tab_bar_h).unwrap_or(40.0))
                };
                let margin = 28.0 * self.win().scale as f32;
                d.outside = my > bar_h + margin || my < -margin || mx < -margin || mx > width + margin || my > height;
                if !d.outside {
                    // Reorder: the slot under the ghost's centre becomes the target.
                    let (ghost_w, ghost_left) = {
                        let w = self.win();
                        let gw = w.tab_hits.iter().find(|h| h.index == d.index).map(|h| h.x1 - h.x0).unwrap_or(80.0);
                        (gw, mx - d.grab_dx)
                    };
                    let center = ghost_left + ghost_w / 2.0;
                    let target = self.win().tab_hits.iter().find(|h| center >= h.x0 && center < h.x1).map(|h| h.index);
                    if let Some(t) = target
                        && t != d.index {
                            self.move_tab(d.index, t);
                            d.index = t;
                        }
                }
                self.win_mut().hover = Hover::Tab(d.index);
                log::debug!("tab drag: index {} outside {} at ({mx:.0},{my:.0})", d.index, d.outside);
                self.set_cursor(if d.outside { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
            self.win_mut().drag = Some(d);
            if d.active {
                self.request_redraw();
                return;
            }
        }

        if self.wins[self.cur].deck.is_open() && self.wins[self.cur].deck.panel_contains(mx, my) {
            if self.wins[self.cur].deck.mouse_move(mx, my) {
                self.request_redraw();
            }
            self.set_cursor(CursorIcon::Default);
            return;
        } else if self.wins[self.cur].deck.mouse_move(-1.0, -1.0) {
            self.request_redraw();
        }
        let hover = self.hover_at(mx, my);
        if hover != self.wins[self.cur].hover {
            self.wins[self.cur].hover = hover;
            self.request_redraw();
        }
        self.update_link_hover();
        let icon = match hover {
            // On a free canvas the card under the pointer decides: resize
            // arrows on an edge, a hand on a title, text over the grid.
            Hover::None if self.on_free_canvas() && self.win().canvas_cursor.is_some() => self.win().canvas_cursor.unwrap_or(CursorIcon::Default),
            Hover::None if self.win().hover_link.is_some() => CursorIcon::Pointer,
            Hover::None if self.wins[self.cur].palette.is_none() => CursorIcon::Text,
            Hover::None => CursorIcon::Default,
            _ => CursorIcon::Pointer,
        };
        self.set_cursor(icon);

        if !self.wins[self.cur].selecting {
            return;
        }
        let Some((point, side)) = self.mouse_point() else { return };
        if let Some(tab) = self.win().active_term() {
            let mut term = tab.term.lock();
            if let Some(sel) = term.selection.as_mut() {
                sel.update(point, side);
            }
        }
        self.request_redraw();
    }

    pub(super) fn set_cursor(&self, icon: CursorIcon) {
        if let Some(w) = self.wins.get(self.cur) {
            w.window.set_cursor(icon);
        }
    }

    /// Encode a wheel notch for a program that asked for mouse events.
    /// Buttons 64/65 are wheel up/down; SGR (1006) is the modern form,
    /// otherwise the classic X10 bytes (or their UTF-8 variant, 1005).
    pub(super) fn wheel_report(mode: TermMode, up: bool, col: usize, row: usize, mods: ModifiersState) -> Option<Vec<u8>> {
        if !mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
            return None;
        }
        let mut btn = if up { 64 } else { 65 };
        if mods.shift_key() {
            btn += 4;
        }
        if mods.alt_key() {
            btn += 8;
        }
        if mods.control_key() {
            btn += 16;
        }
        if mode.contains(TermMode::SGR_MOUSE) {
            return Some(format!("\x1b[<{btn};{};{}M", col + 1, row + 1).into_bytes());
        }
        let mut out = b"\x1b[M".to_vec();
        out.push(32 + btn as u8);
        let (c, r) = (32 + col + 1, 32 + row + 1);
        if mode.contains(TermMode::UTF8_MOUSE) {
            let mut buf = [0u8; 4];
            for v in [c, r] {
                let ch = char::from_u32(v as u32).unwrap_or(' ');
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        } else {
            if c > 223 || r > 223 {
                return None;
            }
            out.push(c as u8);
            out.push(r as u8);
        }
        Some(out)
    }

    /// Turn wheel travel into whole notches for mouse reporting: line
    /// deltas are notches already; pixel deltas accumulate per cell height.
    pub(super) fn wheel_notches(&mut self, delta: MouseScrollDelta, cell_h: f32) -> i32 {
        match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.win_mut().wheel_accum = 0.0;
                if y.abs() < 0.01 { 0 } else { y.round().max(1.0).copysign(y) as i32 }
            }
            MouseScrollDelta::PixelDelta(p) => {
                let w = self.win_mut();
                w.wheel_accum += p.y as f32;
                let n = (w.wheel_accum / cell_h.max(1.0)).trunc();
                w.wheel_accum -= n * cell_h.max(1.0);
                n as i32
            }
        }
    }

    pub(super) fn on_wheel(&mut self, delta: MouseScrollDelta) {
        if self.wins[self.cur].palette.is_some() {
            if let Some(p) = self.wins[self.cur].palette.as_mut() {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y as i32,
                    MouseScrollDelta::PixelDelta(p) => -(p.y / 20.0) as i32,
                };
                p.move_selection(lines);
                self.request_redraw();
            }
            return;
        }
        let Some(l) = self.wins[self.cur].layout else { return };
        if self.wins[self.cur].menu.is_some() {
            return;
        }
        if self.wins[self.cur].deck.is_open() && self.wins[self.cur].deck.panel_contains(self.wins[self.cur].mouse.x as f32, self.wins[self.cur].mouse.y as f32) {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * 40.0,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            self.wins[self.cur].deck.scroll_by(dy);
            self.request_redraw();
            return;
        }
        if self.in_tab_bar(self.wins[self.cur].mouse.y as f32) {
            let dir = match delta {
                MouseScrollDelta::LineDelta(x, y) => (y + x).signum() as i32,
                MouseScrollDelta::PixelDelta(p) => (p.y + p.x).signum() as i32,
            };
            let n = self.wins[self.cur].canvases.len();
            if dir != 0 && n > 1 {
                let next = if dir < 0 { (self.wins[self.cur].active + 1) % n } else { (self.wins[self.cur].active + n - 1) % n };
                self.switch_tab(next);
            }
            return;
        }
        if self.on_free_canvas() && self.canvas_wheel(delta) {
            return;
        }
        // A program that asked for mouse events gets the wheel itself.
        if let Some(tab) = self.win().active_term() {
            let mode = *tab.term.lock().mode();
            if mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                let (col, row) = self.mouse_point().map(|(p, _)| (p.column.0, p.line.0.max(0) as usize)).unwrap_or((0, 0));
                let mods = self.mods;
                let n = self.wheel_notches(delta, l.cell_h);
                if n != 0
                    && let Some(tab) = self.win().active_term()
                {
                    let mut bytes = Vec::new();
                    for _ in 0..n.abs() {
                        if let Some(b) = Self::wheel_report(mode, n > 0, col, row, mods) {
                            bytes.extend_from_slice(&b);
                        }
                    }
                    tab.write(bytes);
                }
                return;
            }
        }
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
            MouseScrollDelta::PixelDelta(p) => (p.y as f32 / l.cell_h) as i32,
        };
        // Alt+wheel: one line per notch, for fine positioning.
        let lines = if self.mods.alt_key() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            dy.signum() as i32
        } else {
            lines
        };
        if lines == 0 {
            return;
        }
        let Some(tab) = self.win().active_term() else { return };
        let mode = *tab.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            // Full-screen apps: turn the wheel into arrow keys.
            let key: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let key = if mode.contains(TermMode::APP_CURSOR) { key.to_vec() } else { key.replace_o() };
            let mut bytes = Vec::new();
            for _ in 0..lines.abs() {
                bytes.extend_from_slice(&key);
            }
            tab.write(bytes);
        } else {
            tab.scroll(Scroll::Delta(lines));
        }
        self.request_redraw();
    }

}

#[cfg(test)]
mod wheel_report_tests {
    use super::*;

    #[test]
    fn sgr_wheel_up_and_down() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(App::wheel_report(mode, true, 4, 9, ModifiersState::empty()), Some(b"\x1b[<64;5;10M".to_vec()));
        assert_eq!(App::wheel_report(mode, false, 0, 0, ModifiersState::empty()), Some(b"\x1b[<65;1;1M".to_vec()));
    }

    #[test]
    fn classic_encoding_and_modifiers() {
        let mode = TermMode::MOUSE_MOTION;
        let alt = ModifiersState::ALT;
        // 32 + 64 + 8 (alt) = 104 = 'h'; column 1 -> 33 '!', row 1 -> 33 '!'
        assert_eq!(App::wheel_report(mode, true, 0, 0, alt), Some(b"\x1b[Mh!!".to_vec()));
    }

    #[test]
    fn silent_without_mouse_mode() {
        assert_eq!(App::wheel_report(TermMode::ALT_SCREEN, true, 0, 0, ModifiersState::empty()), None);
    }
}
