//! Layout and drawing of the Deck, plus UI text helpers shared with the app.

use super::*;

impl Deck {
    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// Draw the gear button into the tab bar. Returns its width.
    pub fn draw_gear(&mut self, fonts: &mut FontSystem, batch: &mut Batch, theme: &Theme, right_x: f32, bar_h: f32, s: f32) -> f32 {
        let size = (26.0 * s).round();
        let x = right_x - size - 12.0 * s;
        let y = ((bar_h - size) / 2.0).round();
        self.gear_rect = Rect { x, y, w: size, h: size };
        let active = self.open;
        if active {
            batch.rrect(x, y, size, size, 6.0 * s, theme.panel);
        }
        batch.outline(x, y, size, size, 6.0 * s, 1.0, if active { theme.accent } else { theme.highlight });
        ui_text_centered(fonts, batch, x, y, size, size, "⚙", 14.0 * s, theme.accent, false);
        size + 24.0 * s
    }

    pub fn draw(&mut self, fonts: &mut FontSystem, batch: &mut Batch, env: &DeckEnv, win_w: f32, win_h: f32, top: f32) {
        let t = self.slide();
        if t <= 0.0 {
            self.rects.clear();
            self.panel_rect = Rect::default();
            return;
        }
        let s = env.scale;
        let theme = env.theme;
        let pw = (PANEL_W * s).round();
        let px = (-(pw) * (1.0 - t)).round();
        let ph = win_h - top;

        // Dim the terminal behind the panel.
        batch.rect(0.0, top, win_w, ph, [0.07, 0.08, 0.1, 0.55 * t]);
        // Panel.
        batch.rect(px, top, pw, ph, theme.panel);
        batch.rect(px + pw - 1.0, top, 1.0, ph, theme.highlight);
        self.panel_rect = Rect { x: px, y: top, w: pw, h: ph };

        let built = self.build(env);
        self.focus_count = built.focusables.len();
        self.focus_shortcuts = built.focusables.iter().map(|f| f.shortcut.clone()).collect();
        self.focus_grid = built.focusables.iter().map(|f| f.grid).collect();
        self.focus_fields = built.focusables.iter().map(|f| f.field).collect();
        if self.page().focus >= built.focusables.len() && !built.focusables.is_empty() {
            self.page_mut().focus = built.focusables.len() - 1;
        }

        // Page push offset.
        let push_t = (self.page().entered.elapsed().as_secs_f32() * 1000.0 / PUSH_MS).min(1.0);
        let push_dx = if self.stack.len() > 1 { (24.0 * s * (1.0 - push_t)).round() } else { 0.0 };

        // Split chrome (top, drawn last) from scrolling content.
        let mut top_chrome: Vec<&W> = Vec::new();
        let mut content: Vec<&W> = Vec::new();
        let mut footer: Option<&W> = None;
        for w in &built.widgets {
            match w {
                W::Header { .. } => top_chrome.push(w),
                W::Search { .. } if self.page().id == PageId::Home => top_chrome.push(w),
                W::Buttons { .. } => footer = Some(w),
                _ => content.push(w),
            }
        }
        let chrome_h: f32 = top_chrome.iter().map(|w| self.widget_height(w, fonts, s, pw)).sum();
        let footer_h = if footer.is_some() { 56.0 * s } else if self.page().id == PageId::Home { 52.0 * s } else { 40.0 * s };
        let content_top = top + chrome_h;
        let content_h = ph - chrome_h - footer_h;
        self.content_rect = Rect { x: px, y: content_top, w: pw, h: content_h };

        // Layout pass: measure content, find focused item's y to keep it visible.
        let mut y_cursor = 0.0f32;
        let mut focused_span: Option<(f32, f32)> = None;
        let focus = self.page().focus;
        let mut heights = Vec::with_capacity(content.len());
        for w in &content {
            let h = self.widget_height(w, fonts, s, pw);
            if let Some((fy, fh)) = self.focus_span_in(w, focus, fonts, s, pw) {
                focused_span = Some((y_cursor + fy, fh));
            }
            heights.push(h);
            y_cursor += h;
        }
        let total_h = y_cursor + 8.0 * s;
        let max_scroll = (total_h - content_h).max(0.0);
        {
            let p = self.page_mut();
            p.scroll = p.scroll.min(max_scroll);
            if p.reveal_focus {
                p.reveal_focus = false;
                if let Some((fy, fh)) = focused_span {
                    if fy < p.scroll {
                        p.scroll = fy.max(0.0);
                    } else if fy + fh > p.scroll + content_h {
                        p.scroll = (fy + fh - content_h).min(max_scroll);
                    }
                }
            }
        }
        let scroll = self.page().scroll;

        // Draw content.
        self.rects.clear();
        let mut y = content_top - scroll;
        let hover = self.hover;
        // Scissor the scrolling content to its viewport.
        batch.push_clip(px.max(0.0), content_top, pw, content_h);
        for (w, h) in content.iter().zip(heights.iter()) {
            if y + h >= content_top - 1.0 && y <= content_top + content_h + 1.0 {
                self.draw_widget(w, fonts, batch, env, px + push_dx, y, pw, s, focus, hover);
            }
            y += h;
        }
        batch.pop_clip();
        // Scrollbar.
        if max_scroll > 0.0 {
            let track_h = content_h;
            let thumb_h = (content_h / total_h * track_h).max(24.0 * s);
            let thumb_y = content_top + (scroll / max_scroll) * (track_h - thumb_h);
            batch.rrect(px + pw - 5.0 * s, thumb_y, 3.0 * s, thumb_h, 2.0 * s, with_alpha(theme.muted, 0.5));
        }
        // Mask overflow above/below with the panel color, then chrome.
        batch.rect(px, top, pw - 1.0, chrome_h, theme.panel);
        batch.rect(px, top + ph - footer_h, pw - 1.0, footer_h, theme.panel);
        let mut cy = top;
        for w in &top_chrome {
            let h = self.widget_height(w, fonts, s, pw);
            self.draw_widget(w, fonts, batch, env, px + push_dx, cy, pw, s, focus, hover);
            cy += h;
        }
        // Footer.
        let fy = top + ph - footer_h;
        batch.rect(px, fy, pw - 1.0, 1.0, theme.highlight);
        if let Some(w) = footer {
            self.draw_widget(w, fonts, batch, env, px + push_dx, fy, pw, s, focus, hover);
        } else if self.page().id == PageId::Home {
            // Quick theme swatches (click to apply) + toggle hint.
            self.swatch_rects.clear();
            let mut sx = px + PAD * s;
            let sw = 22.0 * s;
            let sy = fy + (footer_h - sw) / 2.0;
            let hovered = self.hover_swatch;
            for t in BUILTIN_THEMES.iter().take(3) {
                let bg = crate::renderer::rgb(crate::config::parse_hex(t.bg));
                let cur = theme.name.eq_ignore_ascii_case(t.name);
                let hov = hovered == Some(t.name);
                batch.rrect(sx, sy, sw, sw, 6.0 * s, bg);
                // A dab of the theme's accent so the swatch reads as a theme.
                batch.rrect(sx + sw * 0.55, sy + sw * 0.55, sw * 0.3, sw * 0.3, 3.0 * s, crate::renderer::rgb(crate::config::parse_hex(t.accent)));
                let ring = if cur { theme.accent } else if hov { theme.fg } else { theme.highlight };
                batch.outline(sx, sy, sw, sw, 6.0 * s, if cur || hov { 2.0 } else { 1.0 }, ring);
                self.swatch_rects.push((t.name, Rect { x: sx - 3.0 * s, y: sy - 3.0 * s, w: sw + 6.0 * s, h: sw + 6.0 * s }));
                sx += sw + 8.0 * s;
            }
            if let Some(name) = hovered {
                ui_text(fonts, batch, sx + 4.0 * s, fy + (footer_h - line_h(fonts, 10.0 * s)) / 2.0, name, 10.0 * s, theme.muted, false);
            }
            let hint_w = ui_text_width(fonts, "tap Ctrl+Shift toggles Deck", 10.0 * s);
            let hx = px + pw - PAD * s - hint_w;
            let hy = fy + (footer_h - line_h(fonts, 10.0 * s)) / 2.0;
            let used = ui_text(fonts, batch, hx, hy, "tap Ctrl+Shift", 10.0 * s, theme.accent, false);
            ui_text(fonts, batch, hx + used, hy, " toggles Deck", 10.0 * s, theme.muted, false);
        } else {
            let (l, r) = footer_hints(self.page().id);
            let lh = line_h(fonts, 10.0 * s);
            let hy = fy + (footer_h - lh) / 2.0;
            ui_text(fonts, batch, px + PAD * s, hy, l, 10.0 * s, theme.muted, false);
            let rw = ui_text_width(fonts, r, 10.0 * s);
            ui_text(fonts, batch, px + pw - PAD * s - rw, hy, r, 10.0 * s, theme.muted, false);
        }

        // Toast (bottom of the terminal area, design 2b).
        if let Some((msg, at)) = self.toast.clone() {
            if at.elapsed().as_secs_f32() < 3.0 {
                let alpha = (1.0 - (at.elapsed().as_secs_f32() - 2.4).max(0.0) / 0.6).clamp(0.0, 1.0);
                let tw = ui_text_width(fonts, &msg, 11.0 * s) + 24.0 * s;
                let th = 28.0 * s;
                let tx = (win_w - tw) / 2.0;
                let ty = win_h - th - 12.0 * s;
                batch.rrect(tx, ty, tw, th, 4.0 * s, with_alpha(theme.panel, alpha));
                batch.outline(tx, ty, tw, th, 4.0 * s, 1.0, with_alpha(theme.ansi[2], alpha));
                ui_text(fonts, batch, tx + 12.0 * s, ty + (th - line_h(fonts, 11.0 * s)) / 2.0, &msg, 11.0 * s, with_alpha(theme.fg, alpha), false);
            } else {
                self.toast = None;
            }
        }
    }

    pub(super) fn widget_height(&self, w: &W, fonts: &FontSystem, s: f32, pw: f32) -> f32 {
        match w {
            W::Header { .. } => HEADER_H * s,
            W::Search { .. } => (SEARCH_H + 20.0) * s,
            W::Label { .. } => LABEL_H * s,
            W::Tiles(tiles) => {
                let rows = tiles.len().div_ceil(TILE_COLS);
                rows as f32 * TILE_ROW_H * s + 8.0 * s
            }
            W::Group(rows) => rows.iter().map(|r| self.row_height(r, s)).sum::<f32>() + GROUP_GAP * s,
            W::LetterHeader(_) => 20.0 * s,
            W::ThemeCard { .. } => 106.0 * s,
            W::FontPreview => 102.0 * s,
            W::Field { multiline, value, .. } => {
                let lines = if *multiline { wrap_lines(fonts, value, 11.0 * s, pw - (PAD * 2.0 + 22.0) * s).max(2) } else { 1 };
                (16.0 + 10.0) * s + lines as f32 * 20.0 * s + 12.0 * s + 10.0 * s
            }
            W::GlyphPicker { .. } => 132.0 * s,
            W::Buttons { .. } => 56.0 * s,
            W::Note(_) => 30.0 * s,
            W::CommandStrip(cmd) => {
                let lines = wrap_lines(fonts, cmd, 10.0 * s, pw - (PAD * 2.0 + 16.0) * s).max(1);
                (26.0 + lines as f32 * 16.0 + 14.0) * s
            }
        }
    }

    pub(super) fn row_height(&self, r: &Row, s: f32) -> f32 {
        if r.subtitle.is_empty() { ROW_H * s } else { TALL_ROW_H * s }
    }

    /// (y offset within widget, height) of the focused item, if it's here.
    pub(super) fn focus_span_in(&self, w: &W, focus: usize, _fonts: &FontSystem, s: f32, _pw: f32) -> Option<(f32, f32)> {
        match w {
            W::Tiles(tiles) => {
                let i = tiles.iter().position(|t| t.focus == focus)?;
                let row = i / TILE_COLS;
                let rh = TILE_ROW_H * s;
                Some((row as f32 * rh, rh))
            }
            W::Group(rows) => {
                let mut y = 0.0;
                for r in rows {
                    let h = self.row_height(r, s);
                    if r.focus == Some(focus) {
                        return Some((y, h + GROUP_GAP * s));
                    }
                    y += h;
                }
                None
            }
            W::ThemeCard { focus: f, .. } if *f == focus => Some((0.0, 106.0 * s)),
            W::Field { focus: f, .. } if *f == focus => Some((0.0, 66.0 * s)),
            W::GlyphPicker { focus_glyph, focus_color } if *focus_glyph == focus || *focus_color == focus => Some((0.0, 132.0 * s)),
            W::Buttons { focus: f, .. } if *f == focus || *f + 1 == focus => Some((0.0, 56.0 * s)),
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_widget(&mut self, w: &W, fonts: &mut FontSystem, batch: &mut Batch, env: &DeckEnv, px: f32, y: f32, pw: f32, s: f32, focus: usize, hover: Option<usize>) {
        let theme = env.theme;
        let pad = PAD * s;
        let inner_w = pw - 2.0 * pad;
        match w {
            W::Header { back, title, hint } => {
                let h = HEADER_H * s;
                batch.rect(px, y + h - 1.0, pw - 1.0, 1.0, theme.highlight);
                let lh = line_h(fonts, 12.0 * s);
                let ty = y + (h - lh) / 2.0;
                let mut x = px + 16.0 * s;
                x += ui_text(fonts, batch, x, ty, "‹ ", 13.0 * s, theme.accent, false);
                ui_text(fonts, batch, x, ty, back, 12.0 * s, theme.accent, false);
                ui_text_centered(fonts, batch, px, y, pw, h, title, 13.0 * s, theme.fg, true);
                let hw = ui_text_width(fonts, hint, 10.0 * s);
                ui_text(fonts, batch, px + pw - 14.0 * s - hw, y + (h - line_h(fonts, 10.0 * s)) / 2.0, hint, 10.0 * s, theme.muted, false);
            }
            W::Search { text, placeholder, right } => {
                let bx = px + pad;
                let by = y + 12.0 * s;
                let bh = SEARCH_H * s;
                batch.rrect(bx, by, inner_w, bh, RADIUS_ROW * s, theme.bg);
                batch.outline(bx, by, inner_w, bh, RADIUS_ROW * s, 1.0, theme.accent);
                let lh = line_h(fonts, 12.0 * s);
                let ty = by + (bh - lh) / 2.0;
                let mut x = bx + 10.0 * s;
                x += ui_text(fonts, batch, x, ty, "❯ ", 13.0 * s, theme.accent, false);
                let rw = if right.is_empty() { 0.0 } else { ui_text_width(fonts, right, 10.0 * s) + 10.0 * s };
                let avail = inner_w - (x - bx) - rw - 20.0 * s;
                if text.is_empty() {
                    ui_text_clipped(fonts, batch, x, ty, placeholder, 12.0 * s, theme.muted, false, avail);
                    // caret
                    batch.rect(x, by + 8.0 * s, 7.0 * s, bh - 16.0 * s, theme.cursor);
                } else {
                    let used = ui_text_clipped(fonts, batch, x, ty, text, 12.0 * s, theme.fg, false, avail);
                    batch.rect(x + used + 1.0, by + 8.0 * s, 7.0 * s, bh - 16.0 * s, theme.cursor);
                }
                if !right.is_empty() {
                    ui_text(fonts, batch, bx + inner_w - 10.0 * s - (rw - 10.0 * s), by + (bh - line_h(fonts, 10.0 * s)) / 2.0, right, 10.0 * s, theme.muted, false);
                }
            }
            W::Label { text, right } => {
                let lh = line_h(fonts, 10.0 * s);
                let ty = y + LABEL_H * s - lh - 5.0 * s;
                ui_text_tracked(fonts, batch, px + pad, ty, text, 10.0 * s, theme.muted, 0.14);
                if !right.is_empty() {
                    let rw = ui_text_width(fonts, right, 10.0 * s);
                    ui_text(fonts, batch, px + pw - pad - rw, ty, right, 10.0 * s, theme.muted, false);
                }
            }
            W::LetterHeader(l) => {
                let h = 20.0 * s;
                batch.rect(px + pad, y, inner_w, h, theme.panel);
                batch.rect(px + pad, y + h - 1.0, inner_w, 1.0, theme.highlight);
                let lh = line_h(fonts, 9.0 * s);
                ui_text(fonts, batch, px + pad + 9.0 * s, y + (h - lh) / 2.0, l, 9.0 * s, theme.accent, false);
            }
            W::Tiles(tiles) => {
                let cell_w = inner_w / TILE_COLS as f32;
                let tile = TILE * s;
                let row_h = TILE_ROW_H * s;
                for (i, t) in tiles.iter().enumerate() {
                    let col = i % TILE_COLS;
                    let row = i / TILE_COLS;
                    let cx = px + pad + col as f32 * cell_w + (cell_w - tile) / 2.0;
                    let cy = y + 8.0 * s + row as f32 * row_h;
                    let focused = t.focus == focus;
                    let hovered = hover == Some(t.focus);
                    let (tx, ty, tw) = if focused {
                        let grow = tile * 0.04;
                        (cx - grow / 2.0, cy - grow / 2.0, tile + grow)
                    } else {
                        (cx, cy, tile)
                    };
                    let color = theme.ansi[t.badge.color as usize];
                    batch.rrect(tx, ty, tw, tw, RADIUS_BADGE * s, if hovered && !focused { scale_rgb(color, 1.12) } else { color });
                    if focused {
                        batch.outline(tx - 3.0 * s, ty - 3.0 * s, tw + 6.0 * s, tw + 6.0 * s, (RADIUS_BADGE + 3.0) * s, 2.0, theme.accent);
                    }
                    ui_text_centered(fonts, batch, tx, ty, tw, tw, &t.badge.glyph.to_string(), 23.0 * s, theme.badge_ink, false);
                    let label_w = ui_text_width(fonts, &t.label, 10.0 * s).min(cell_w - 4.0 * s);
                    let lx = px + pad + col as f32 * cell_w + (cell_w - label_w) / 2.0;
                    ui_text_clipped(fonts, batch, lx, cy + tile + 8.0 * s, &t.label, 10.0 * s, if focused { theme.fg } else { scale_rgb(theme.fg, 0.8) }, false, cell_w - 4.0 * s);
                    self.rects.push((t.focus, Rect { x: px + pad + col as f32 * cell_w, y: cy - 4.0 * s, w: cell_w, h: row_h }));
                }
            }
            W::Group(rows) => {
                let gh: f32 = rows.iter().map(|r| self.row_height(r, s)).sum();
                let gx = px + pad;
                batch.rrect(gx, y, inner_w, gh, RADIUS_ROW * s, theme.bg);
                batch.outline(gx, y, inner_w, gh, RADIUS_ROW * s, 1.0, theme.highlight);
                let mut ry = y;
                for (i, r) in rows.iter().enumerate() {
                    let rh = self.row_height(r, s);
                    let focused = r.focus.is_some() && r.focus == Some(focus);
                    let hovered = r.focus.is_some() && r.focus == hover;
                    if focused {
                        batch.rect(gx + 1.0, ry + 1.0, inner_w - 2.0, rh - 1.0, theme.hover);
                        batch.rect(gx + 1.0, ry + 1.0, 3.0 * s, rh - 1.0, theme.accent);
                    } else if hovered {
                        batch.rect(gx + 1.0, ry + 1.0, inner_w - 2.0, rh - 1.0, with_alpha(theme.hover, 0.7));
                    }
                    if i + 1 < rows.len() {
                        batch.rect(gx + 1.0, ry + rh - 1.0, inner_w - 2.0, 1.0, theme.panel);
                    }
                    let mut x = gx + 12.0 * s;
                    if let Some(b) = &r.badge {
                        let bs = BADGE * s;
                        let by = ry + (rh - bs) / 2.0;
                        batch.rrect(x, by, bs, bs, RADIUS_BADGE * s, theme.ansi[b.color as usize]);
                        ui_text_centered(fonts, batch, x, by, bs, bs, &b.glyph.to_string(), 12.0 * s, theme.badge_ink, false);
                        x += bs + 12.0 * s;
                    }
                    let fg = if r.danger { theme.ansi[1] } else if !r.enabled { with_alpha(theme.muted, 0.7) } else { theme.fg };
                    // Right side: value + kind indicator.
                    let mut right_x = gx + inner_w - 12.0 * s;
                    let vh = line_h(fonts, 11.0 * s);
                    match &r.kind {
                        RowKind::Chevron => {
                            let cw = ui_text_width(fonts, "›", 12.0 * s);
                            right_x -= cw;
                            ui_text(fonts, batch, right_x, ry + (rh - line_h(fonts, 12.0 * s)) / 2.0, "›", 12.0 * s, theme.muted, false);
                            right_x -= 8.0 * s;
                        }
                        RowKind::Toggle(on) => {
                            let tw = 34.0 * s;
                            let th = 18.0 * s;
                            right_x -= tw;
                            let ty = ry + (rh - th) / 2.0;
                            let (track, knob) = if !r.enabled {
                                (with_alpha(theme.highlight, 0.5), with_alpha(theme.muted, 0.5))
                            } else if *on {
                                (theme.accent, theme.tab_bar)
                            } else {
                                (theme.highlight, theme.muted)
                            };
                            batch.rrect(right_x, ty, tw, th, 6.0 * s, track);
                            let kx = if *on { right_x + tw - 2.0 * s - 14.0 * s } else { right_x + 2.0 * s };
                            batch.rrect(kx, ty + 2.0 * s, 14.0 * s, 14.0 * s, 4.0 * s, knob);
                            right_x -= 10.0 * s;
                        }
                        RowKind::Stepper(v) => {
                            // − [value] +
                            let bw = 24.0 * s;
                            let vw = (ui_text_width(fonts, v, 12.0 * s) + 16.0 * s).max(34.0 * s);
                            let th = 24.0 * s;
                            let total = bw * 2.0 + vw;
                            right_x -= total;
                            let ty = ry + (rh - th) / 2.0;
                            batch.outline(right_x, ty, total, th, 4.0 * s, 1.0, theme.highlight);
                            batch.rect(right_x + bw, ty, 1.0, th, theme.highlight);
                            batch.rect(right_x + bw + vw, ty, 1.0, th, theme.highlight);
                            if focused {
                                batch.rrect(right_x + bw + vw + 1.0, ty + 1.0, bw - 2.0, th - 2.0, 3.0 * s, theme.hover);
                            }
                            ui_text_centered(fonts, batch, right_x, ty, bw, th, "−", 12.0 * s, scale_rgb(theme.fg, 0.8), false);
                            ui_text_centered(fonts, batch, right_x + bw, ty, vw, th, v, 12.0 * s, theme.fg, false);
                            ui_text_centered(fonts, batch, right_x + bw + vw, ty, bw, th, "+", 12.0 * s, theme.accent, false);
                            right_x -= 10.0 * s;
                        }
                        RowKind::Check(on) => {
                            if *on {
                                let cw = ui_text_width(fonts, "✓", 12.0 * s);
                                right_x -= cw;
                                ui_text(fonts, batch, right_x, ry + (rh - line_h(fonts, 12.0 * s)) / 2.0, "✓", 12.0 * s, theme.accent, false);
                                right_x -= 8.0 * s;
                            }
                        }
                        RowKind::Hint(h) => {
                            let hw = ui_text_width(fonts, h, 10.0 * s);
                            right_x -= hw;
                            ui_text(fonts, batch, right_x, ry + (rh - line_h(fonts, 10.0 * s)) / 2.0, h, 10.0 * s, theme.accent, false);
                            right_x -= 8.0 * s;
                        }
                        RowKind::Info => {}
                    }
                    if !r.value.is_empty() && !matches!(r.kind, RowKind::Stepper(_)) {
                        let max_vw = (inner_w * 0.45).max(40.0 * s);
                        let vw = ui_text_width(fonts, &r.value, 11.0 * s).min(max_vw);
                        right_x -= vw;
                        let vy = if r.subtitle.is_empty() { ry + (rh - vh) / 2.0 } else { ry + 6.0 * s };
                        ui_text_clipped(fonts, batch, right_x, vy, &r.value, 11.0 * s, theme.muted, false, max_vw);
                        right_x -= 8.0 * s;
                    }
                    // Title (+ subtitle).
                    let title_w = (right_x - x).max(20.0 * s);
                    if r.subtitle.is_empty() {
                        let lh = line_h(fonts, 12.0 * s);
                        ui_text_hi(fonts, batch, x, ry + (rh - lh) / 2.0, &r.title, &r.hi, 12.0 * s, fg, theme.accent, title_w);
                    } else {
                        ui_text_hi(fonts, batch, x, ry + 5.0 * s, &r.title, &r.hi, 12.0 * s, fg, theme.accent, title_w);
                        ui_text_clipped(fonts, batch, x, ry + 5.0 * s + 18.0 * s, &r.subtitle, 9.0 * s, theme.muted, false, title_w);
                    }
                    if let Some(f) = r.focus {
                        self.rects.push((f, Rect { x: gx, y: ry, w: inner_w, h: rh }));
                    }
                    ry += rh;
                }
            }
            W::ThemeCard { idx, selected, focus: f } => {
                let t = &BUILTIN_THEMES[*idx];
                let hex = |h: &str| crate::renderer::rgb(crate::config::parse_hex(h));
                let card_h = 96.0 * s;
                let cx = px + pad;
                let focused = *f == focus;
                let hovered = hover == Some(*f);
                let bg = hex(t.bg);
                batch.rrect(cx, y, inner_w, card_h, 6.0 * s, bg);
                let border = if focused { theme.accent } else if hovered { theme.muted } else { theme.highlight };
                batch.outline(cx, y, inner_w, card_h, 6.0 * s, if focused { 2.0 } else { 1.0 }, border);
                let fg = hex(t.fg);
                let muted = hex(t.muted);
                let ix = cx + 10.0 * s;
                let mut iy = y + 8.0 * s;
                let mut x = ui_text(fonts, batch, ix, iy, t.name, 11.0 * s, fg, true);
                x += ui_text(fonts, batch, ix + x, iy, &format!(" · {}", t.tag), 10.0 * s, muted, false);
                let _ = x;
                if *selected {
                    let cw = ui_text_width(fonts, "✓", 12.0 * s);
                    ui_text(fonts, batch, cx + inner_w - 8.0 * s - cw, iy, "✓", 12.0 * s, hex(t.accent), false);
                }
                iy += 19.0 * s;
                // Mini terminal sample in the theme's own colors.
                let sz = 8.5 * s;
                let lh = 13.0 * s;
                let a = t.ansi;
                let mut lx = ix;
                lx += ui_text(fonts, batch, lx, iy, "dev@kindly", sz, hex(a[2]), false);
                lx += ui_text(fonts, batch, lx, iy, ":", sz, muted, false);
                lx += ui_text(fonts, batch, lx, iy, "~", sz, hex(a[4]), false);
                lx += ui_text(fonts, batch, lx, iy, "$ ", sz, muted, false);
                ui_text(fonts, batch, lx, iy, "git status -sb", sz, fg, false);
                iy += lh;
                ui_text(fonts, batch, ix, iy, "## main...origin/main", sz, hex(a[3]), false);
                iy += lh;
                let mut lx = ix;
                lx += ui_text(fonts, batch, lx, iy, "M", sz, hex(a[2]), false);
                ui_text(fonts, batch, lx, iy, " src/render/deck.rs", sz, fg, false);
                iy += lh;
                let used = ui_text(fonts, batch, ix, iy, "$ ", sz, muted, false);
                batch.rect(ix + used, iy + 1.0, 5.0 * s, 9.0 * s, hex(t.cursor));
                // Swatch strip.
                let sy = y + card_h - 8.0 * s - 8.0 * s;
                let sw = (inner_w - 20.0 * s - 15.0 * 2.0 * s) / 16.0;
                for (i, c) in a.iter().enumerate() {
                    batch.rect(ix + i as f32 * (sw + 2.0 * s), sy, sw, 8.0 * s, hex(c));
                }
                self.rects.push((*f, Rect { x: cx, y, w: inner_w, h: card_h }));
            }
            W::FontPreview => {
                let bx = px + pad;
                let by = y + 12.0 * s;
                let bh = 88.0 * s;
                batch.rrect(bx, by, inner_w, bh, RADIUS_ROW * s, theme.bg);
                batch.outline(bx, by, inner_w, bh, RADIUS_ROW * s, 1.0, theme.highlight);
                let ix = bx + 12.0 * s;
                let mut iy = by + 9.0 * s;
                ui_text_tracked(fonts, batch, ix, iy, &format!("PREVIEW · {} / {} pt", env.font_family, env.config.font.size), 9.0 * s, theme.muted, 0.14);
                iy += 19.0 * s;
                // Draw at the terminal's real cell size so it matches the grid.
                let m = fonts.metrics;
                let a = theme.ansi;
                let mut lx = ix;
                lx += cell_text(fonts, batch, lx, iy, "dev@kindly", a[2]);
                lx += cell_text(fonts, batch, lx, iy, ":", theme.muted);
                lx += cell_text(fonts, batch, lx, iy, "~", a[4]);
                lx += cell_text(fonts, batch, lx, iy, "$ ", theme.muted);
                cell_text(fonts, batch, lx, iy, "ls -la", theme.fg);
                iy += m.height;
                cell_text(fonts, batch, ix, iy, "┃ 0O1lI| → λ ≠ ≈ 🚀", scale_rgb(theme.fg, 0.85));
                iy += m.height;
                let mut lx = ix;
                lx += cell_text(fonts, batch, lx, iy, "fn ", a[3]);
                lx += cell_text(fonts, batch, lx, iy, "main", a[4]);
                lx += cell_text(fonts, batch, lx, iy, "() -> ", theme.fg);
                lx += cell_text(fonts, batch, lx, iy, "Result", a[5]);
                lx += cell_text(fonts, batch, lx, iy, "<()> { ", theme.fg);
                batch.rect(lx, iy, m.width, m.height, theme.cursor);
            }
            W::Field { label, value, placeholder, focus: f, multiline, active, right } => {
                let lh9 = line_h(fonts, 9.0 * s);
                ui_text_tracked(fonts, batch, px + pad, y + 4.0 * s, label, 9.0 * s, theme.muted, 0.14);
                if !right.is_empty() {
                    let rw = ui_text_width(fonts, right, 9.0 * s) + 12.0 * s;
                    let rx = px + pw - pad - rw;
                    if right.starts_with("HOST") {
                        batch.outline(rx, y + 1.0 * s, rw, lh9 + 4.0 * s, 4.0 * s, 1.0, theme.ansi[2]);
                        ui_text(fonts, batch, rx + 6.0 * s, y + 3.0 * s, right, 9.0 * s, theme.ansi[2], false);
                    } else {
                        ui_text(fonts, batch, rx + 6.0 * s, y + 3.0 * s, right, 9.0 * s, theme.muted, false);
                    }
                }
                let by = y + 4.0 * s + lh9 + 8.0 * s;
                let lines = if *multiline { wrap_lines(fonts, value, 11.0 * s, inner_w - 22.0 * s).max(2) } else { 1 };
                let bh = if *multiline { (lines as f32 * 20.0 + 12.0) * s } else { 32.0 * s };
                let focused = *f == focus || *active;
                batch.rrect(px + pad, by, inner_w, bh, RADIUS_ROW * s, theme.bg);
                batch.outline(px + pad, by, inner_w, bh, RADIUS_ROW * s, 1.0, if focused { theme.accent } else if hover == Some(*f) { theme.muted } else { theme.highlight });
                let tx = px + pad + 12.0 * s;
                let caret_h = 17.0 * s;
                if value.is_empty() {
                    let ty = if *multiline { by + 6.0 * s } else { by + (bh - line_h(fonts, 12.0 * s)) / 2.0 };
                    ui_text_clipped(fonts, batch, tx, ty, placeholder, 12.0 * s, with_alpha(theme.muted, 0.8), false, inner_w - 22.0 * s);
                    if focused {
                        batch.rect(tx, if *multiline { by + 6.0 * s } else { by + (bh - caret_h) / 2.0 }, 7.0 * s, caret_h, theme.cursor);
                    }
                } else if *multiline {
                    let mut ly = by + 6.0 * s;
                    let wrapped = wrap_text(fonts, value, 11.0 * s, inner_w - 22.0 * s);
                    let last = wrapped.len().saturating_sub(1);
                    for (i, line) in wrapped.iter().enumerate() {
                        let used = ui_text(fonts, batch, tx, ly, line, 11.0 * s, theme.fg, false);
                        if i == last && focused {
                            batch.rect(tx + used + 1.0, ly, 7.0 * s, caret_h, theme.cursor);
                        }
                        ly += 20.0 * s;
                    }
                } else {
                    let ty = by + (bh - line_h(fonts, 12.0 * s)) / 2.0;
                    let used = ui_text_clipped(fonts, batch, tx, ty, value, 12.0 * s, theme.fg, false, inner_w - 32.0 * s);
                    if focused {
                        batch.rect(tx + used + 1.0, by + (bh - caret_h) / 2.0, 7.0 * s, caret_h, theme.cursor);
                    }
                }
                self.rects.push((*f, Rect { x: px + pad, y: by, w: inner_w, h: bh }));
            }
            W::GlyphPicker { focus_glyph, focus_color } => {
                let Some(ed) = self.editor.as_ref() else { return };
                let big = 64.0 * s;
                let color = theme.ansi[BADGE_COLORS[ed.color_idx] as usize];
                let glyph = ICON_SHEET[ed.glyph_idx].0;
                batch.rrect(px + pad, y + 6.0 * s, big, big, 6.0 * s, color);
                if focus == *focus_glyph || focus == *focus_color {
                    batch.outline(px + pad - 3.0 * s, y + 3.0 * s, big + 6.0 * s, big + 6.0 * s, 9.0 * s, 2.0, theme.accent);
                }
                ui_text_centered(fonts, batch, px + pad, y + 6.0 * s, big, big, &glyph.to_string(), 28.0 * s, theme.badge_ink, false);
                let gx = px + pad + big + 14.0 * s;
                let gw = inner_w - big - 14.0 * s;
                ui_text_tracked(fonts, batch, gx, y + 4.0 * s, "GLYPH", 9.0 * s, theme.muted, 0.14);
                let cols = 8;
                let cell = (gw - 7.0 * 4.0 * s) / cols as f32;
                let ch = 24.0 * s;
                let gy = y + 20.0 * s;
                for (i, (g, _, _)) in ICON_SHEET.iter().enumerate() {
                    let cx = gx + (i % cols) as f32 * (cell + 4.0 * s);
                    let cy = gy + (i / cols) as f32 * (ch + 4.0 * s);
                    let sel = i == ed.glyph_idx;
                    batch.rrect(cx, cy, cell, ch, 4.0 * s, if sel { theme.highlight } else { theme.bg });
                    if sel {
                        batch.outline(cx, cy, cell, ch, 4.0 * s, 1.0, if focus == *focus_glyph { theme.accent } else { theme.muted });
                    }
                    ui_text_centered(fonts, batch, cx, cy, cell, ch, &g.to_string(), 12.0 * s, if sel { theme.fg } else { scale_rgb(theme.fg, 0.8) }, false);
                }
                let glyph_rows_h = 2.0 * (ch + 4.0 * s);
                self.rects.push((*focus_glyph, Rect { x: gx, y: gy, w: gw, h: glyph_rows_h }));
                let cy = gy + glyph_rows_h + 6.0 * s;
                ui_text_tracked(fonts, batch, gx, cy, "BADGE COLOR", 9.0 * s, theme.muted, 0.14);
                let sy = cy + 16.0 * s;
                let sw = (gw - (BADGE_COLORS.len() as f32 - 1.0) * 4.0 * s) / BADGE_COLORS.len() as f32;
                for (i, c) in BADGE_COLORS.iter().enumerate() {
                    let sx = gx + i as f32 * (sw + 4.0 * s);
                    batch.rrect(sx, sy, sw, 14.0 * s, 4.0 * s, theme.ansi[*c as usize]);
                    if i == ed.color_idx {
                        batch.outline(sx - 2.0 * s, sy - 2.0 * s, sw + 4.0 * s, 18.0 * s, 5.0 * s, 2.0, if focus == *focus_color { theme.accent } else { theme.muted });
                    }
                }
                self.rects.push((*focus_color, Rect { x: gx, y: sy - 4.0 * s, w: gw, h: 22.0 * s }));
                let _ = ed;
            }
            W::Buttons { cancel, save, focus: f } => {
                let h = 56.0 * s;
                let bh = 34.0 * s;
                let by = y + (h - bh) / 2.0;
                let gap = 10.0 * s;
                let bw = (inner_w - gap) / 2.0;
                let cx = px + pad;
                let sx = cx + bw + gap;
                let c_focus = focus == *f;
                let s_focus = focus == *f + 1;
                batch.outline(cx, by, bw, bh, 4.0 * s, if c_focus { 2.0 } else { 1.0 }, if c_focus { theme.accent } else { theme.highlight });
                if hover == Some(*f) {
                    batch.rrect(cx, by, bw, bh, 4.0 * s, with_alpha(theme.hover, 0.7));
                }
                ui_text_centered(fonts, batch, cx, by, bw, bh, &format!("{cancel}  Esc"), 12.0 * s, scale_rgb(theme.fg, 0.85), false);
                batch.rrect(sx, by, bw, bh, 4.0 * s, if hover == Some(*f + 1) { scale_rgb(theme.accent, 1.1) } else { theme.accent });
                if s_focus {
                    batch.outline(sx - 3.0 * s, by - 3.0 * s, bw + 6.0 * s, bh + 6.0 * s, 7.0 * s, 2.0, theme.accent);
                }
                ui_text_centered(fonts, batch, sx, by, bw, bh, &format!("{save}  ⏎"), 12.0 * s, theme.badge_ink, true);
                self.rects.push((*f, Rect { x: cx, y: by, w: bw, h: bh }));
                self.rects.push((*f + 1, Rect { x: sx, y: by, w: bw, h: bh }));
            }
            W::Note(text) => {
                ui_text_clipped(fonts, batch, px + pad, y + 9.0 * s, text, 9.0 * s, theme.muted, false, inner_w);
            }
            W::CommandStrip(cmd) => {
                let bx = px + pad;
                let lines = wrap_text(fonts, cmd, 10.0 * s, inner_w - 16.0 * s);
                let bh = (26.0 + lines.len() as f32 * 16.0) * s;
                batch.rrect(bx, y, inner_w, bh, RADIUS_ROW * s, theme.bg);
                batch.outline(bx, y, inner_w, bh, RADIUS_ROW * s, 1.0, theme.highlight);
                ui_text_tracked(fonts, batch, bx + 10.0 * s, y + 7.0 * s, "SELECTED", 8.0 * s, theme.muted, 0.14);
                let mut ly = y + 22.0 * s;
                for (i, line) in lines.iter().enumerate() {
                    if i == 0 {
                        // Color the program name like the design.
                        let (head, rest) = line.split_once(' ').map(|(a, b)| (a.to_string(), format!(" {b}"))).unwrap_or((line.clone(), String::new()));
                        let used = ui_text(fonts, batch, bx + 10.0 * s, ly, &head, 10.0 * s, theme.ansi[2], false);
                        ui_text(fonts, batch, bx + 10.0 * s + used, ly, &rest, 10.0 * s, theme.fg, false);
                    } else {
                        ui_text(fonts, batch, bx + 10.0 * s, ly, line, 10.0 * s, theme.fg, false);
                    }
                    ly += 16.0 * s;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Text helpers (UI text at arbitrary sizes, drawn from the glyph atlas)
// ---------------------------------------------------------------------------

/// Line height of UI text at a design size (after TEXT_SCALE).
pub(super) fn line_h(fonts: &FontSystem, size: f32) -> f32 {
    fonts.line_height_for(size * TEXT_SCALE)
}

pub fn ui_text(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, text: &str, size: f32, color: Rgba, bold: bool) -> f32 {
    let size = size * TEXT_SCALE;
    let adv = fonts.advance_for(size);
    let asc = fonts.ascent_for(size);
    let mut cx = x;
    for ch in text.chars() {
        if let Some(g) = fonts.glyph(GlyphKey::sized(ch, size, bold)) {
            batch.glyph(cx, y + asc, &g, color);
        }
        cx += adv;
    }
    cx - x
}

pub fn ui_text_width(fonts: &FontSystem, text: &str, size: f32) -> f32 {
    fonts.advance_for(size * TEXT_SCALE) * text.chars().count() as f32
}

pub(super) fn ui_text_clipped(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, text: &str, size: f32, color: Rgba, bold: bool, max_w: f32) -> f32 {
    let adv = fonts.advance_for(size * TEXT_SCALE);
    let max_chars = (max_w / adv).floor().max(0.0) as usize;
    let n = text.chars().count();
    if n <= max_chars {
        return ui_text(fonts, batch, x, y, text, size, color, bold);
    }
    if max_chars == 0 {
        return 0.0;
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    ui_text(fonts, batch, x, y, &format!("{cut}…"), size, color, bold)
}

/// Text with certain character positions in a highlight color.
#[allow(clippy::too_many_arguments)]
pub(super) fn ui_text_hi(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, text: &str, hi: &[usize], size: f32, color: Rgba, hi_color: Rgba, max_w: f32) {
    if hi.is_empty() {
        ui_text_clipped(fonts, batch, x, y, text, size, color, false, max_w);
        return;
    }
    let size = size * TEXT_SCALE;
    let adv = fonts.advance_for(size);
    let asc = fonts.ascent_for(size);
    let max_chars = (max_w / adv).floor().max(1.0) as usize;
    let mut cx = x;
    for (i, ch) in text.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        let c = if hi.contains(&i) { hi_color } else { color };
        if let Some(g) = fonts.glyph(GlyphKey::sized(ch, size, false)) {
            batch.glyph(cx, y + asc, &g, c);
        }
        cx += adv;
    }
}

pub(super) fn ui_text_centered(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, w: f32, h: f32, text: &str, size: f32, color: Rgba, bold: bool) {
    let tw = ui_text_width(fonts, text, size);
    let lh = line_h(fonts, size);
    ui_text(fonts, batch, (x + (w - tw) / 2.0).round(), (y + (h - lh) / 2.0).round(), text, size, color, bold);
}

/// Public wrapper for letter-spaced caps labels used outside the Deck.
pub fn ui_text_tracked_pub(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, text: &str, size: f32, color: Rgba) -> f32 {
    ui_text_tracked(fonts, batch, x, y, text, size, color, 0.14)
}

/// Letter-spaced caps label (design: +0.14em tracking).
pub(super) fn ui_text_tracked(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, text: &str, size: f32, color: Rgba, tracking: f32) -> f32 {
    let size = size * TEXT_SCALE;
    let adv = fonts.advance_for(size) + size * tracking;
    let asc = fonts.ascent_for(size);
    let mut cx = x;
    for ch in text.chars() {
        if let Some(g) = fonts.glyph(GlyphKey::sized(ch, size, false)) {
            batch.glyph(cx, y + asc, &g, color);
        }
        cx += adv;
    }
    cx - x
}

/// Text at the terminal's own cell metrics (font preview).
pub(super) fn cell_text(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, text: &str, color: Rgba) -> f32 {
    let m = fonts.metrics;
    let mut cx = x;
    for ch in text.chars() {
        if let Some(g) = fonts.glyph(GlyphKey::cell(ch, false, false)) {
            batch.glyph(cx, y + m.ascent, &g, color);
        }
        cx += m.width;
    }
    cx - x
}

pub(super) fn wrap_text(fonts: &FontSystem, text: &str, size: f32, max_w: f32) -> Vec<String> {
    let adv = fonts.advance_for(size * TEXT_SCALE);
    let cols = (max_w / adv).floor().max(4.0) as usize;
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let end = (start + cols).min(chars.len());
            lines.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    lines
}

pub(super) fn wrap_lines(fonts: &FontSystem, text: &str, size: f32, max_w: f32) -> usize {
    wrap_text(fonts, text, size, max_w).len()
}

