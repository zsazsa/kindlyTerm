//! Per-frame drawing: tab bar, terminal grid, overlays.

use super::*;
use crate::effects::EffectsConfig;
use crate::terminal::Terminal;

impl App {
    pub(super) fn draw(&mut self) -> Result<()> {
        let Some(l) = self.wins[self.cur].layout else { return Ok(()) };
        self.wins[self.cur].batch.clear();
        // Tab bar after the Deck so scrolled Deck content never spills onto it.
        self.draw_terminal(l);
        self.draw_deck(l);
        self.draw_tab_bar(l);
        self.draw_palette(l);
        self.draw_menu(l);
        self.draw_cheat(l);
        self.wins[self.cur].frame += 1;
        let actions_frame = if self.debug.actions_frame > 0 { self.debug.actions_frame } else { self.debug.screenshot_frame.saturating_sub(1) };
        if self.wins[self.cur].frame == actions_frame && self.debug.actions_after.is_none() && !self.debug.actions.is_empty() {
            self.run_debug_actions(None);
            // Rebuild the batch with the new state.
            self.wins[self.cur].batch.clear();
            self.draw_terminal(l);
            self.draw_deck(l);
            self.draw_tab_bar(l);
            self.draw_palette(l);
            self.draw_menu(l);
            self.draw_cheat(l);
        }
        if self.wins[self.cur].deck.animating() {
            self.request_redraw();
        }
        let bg = with_alpha(self.theme.bg, self.config.colors.opacity.clamp(0.3, 1.0));
        let shot = self.debug.screenshot.clone().filter(|_| {
            self.debug.screenshot_frame > 0 && self.wins[self.cur].frame + 1 == self.debug.screenshot_frame && !self.debug.shot_done
        });
        let w = &mut self.wins[self.cur];
        if let Some(path) = shot {
            w.renderer.screenshot(&mut w.fonts, &w.batch, bg, &path)?;
            self.debug.shot_done = true;
            log::info!("wrote screenshot {}", path.display());
        }
        w.renderer.render(&mut w.fonts, &w.batch, bg)
    }

    /// Draw a string at pixel (x, y) (top-left of the row). Returns the width used.
    pub(super) fn text(fonts: &mut FontSystem, batch: &mut Batch, x: f32, y: f32, s: &str, color: Rgba, bold: bool, max_cells: usize) -> f32 {
        let m = fonts.metrics;
        let mut cx = x;
        for (i, ch) in s.chars().enumerate() {
            if i >= max_cells {
                break;
            }
            if let Some(g) = fonts.glyph(GlyphKey::cell(ch, bold, false)) {
                batch.glyph(cx, y + m.ascent, &g, color);
            }
            cx += m.width;
        }
        cx - x
    }

    pub(super) fn draw_tab_bar(&mut self, l: Layout) {
        let theme = &self.theme;
        let opacity = self.config.colors.opacity.clamp(0.3, 1.0);
        let n_shortcuts = self.store.commands.len();
        let w = &mut self.wins[self.cur];
        let titles: Vec<(String, bool)> = (0..w.canvases.len()).map(|i| (w.tab_title(i), !w.canvases[i].is_single())).collect();
        let scroll_off = w.active_term().map(|t| t.term.lock().grid().display_offset()).unwrap_or(0);
        let fonts = &mut w.fonts;
        let batch = &mut w.batch;
        let s = w.scale as f32;
        let width = w.renderer.width as f32;
        let bar_h = l.tab_bar_h;
        batch.rect(0.0, 0.0, width, bar_h, with_alpha(theme.tab_bar, opacity));
        batch.rect(0.0, bar_h - 1.0, width, 1.0, theme.highlight);

        // Design tokens: 12 px labels, 14 px side padding, 9 px gaps.
        let font = 12.0;
        let adv = ui_text_width(fonts, "M", font);
        let lh = fonts.line_height_for(font * 1.12);
        let text_y = ((bar_h - lh) / 2.0).floor();
        let pad_x = 14.0 * s;
        let gap = 9.0 * s;
        let close_w = adv + gap;

        w.tab_hits.clear();
        w.plus_hit = None;

        // Shrink titles when there are many tabs so they all fit.
        let plus_w = 32.0 * s;
        let gear_w = 50.0 * s;
        let avail = width - l.pad - plus_w - gear_w - 8.0 * s;
        let n = w.canvases.len().max(1) as f32;
        let per_tab_chars = ((avail / n - 2.0 * pad_x - close_w - 2.0 * gap) / adv).floor() as usize;
        let max_title = per_tab_chars.saturating_sub(2).clamp(4, 32);

        // Layout pass: widths and x positions.
        struct T {
            label: String,
            /// Characters of `label` that are real text (rest is width padding).
            text_chars: usize,
            num: String,
            x: f32,
            w: f32,
        }
        let rename = w.rename.clone();
        let mut tabs: Vec<T> = Vec::with_capacity(titles.len());
        let mut x = l.pad.min(8.0 * s);
        for (i, (base_title, is_canvas)) in titles.iter().enumerate() {
            let renaming = rename.as_ref().map(|(ri, _, _)| *ri == i).unwrap_or(false);
            let is_canvas = *is_canvas;
            let mut title = match &rename {
                Some((ri, text, _)) if *ri == i => text.clone(),
                _ => base_title.clone(),
            };
            if is_canvas && !renaming {
                title = format!("▦ {title}");
            }
            let text_chars = title.chars().count();
            if renaming {
                // Keep some room so the tab does not collapse while typing.
                let pad_chars = 6usize.saturating_sub(text_chars);
                title.push_str(&" ".repeat(pad_chars));
            } else if text_chars > max_title {
                title = format!("{}…", title.chars().take(max_title - 1).collect::<String>());
            }
            let num = (i + 1).to_string();
            let tw = pad_x + ui_text_width(fonts, &num, font) + gap + ui_text_width(fonts, &title, font) + gap + close_w + pad_x;
            tabs.push(T { label: title, text_chars, num, x, w: tw });
            x += tw;
        }
        let tabs_end = x;

        // While dragging, the dragged tab follows the pointer and the others
        // are drawn in their (already reordered) slots.
        let drag = w.drag.filter(|d| d.active);
        let ghost_x = drag.map(|d| (w.mouse.x as f32 - d.grab_dx).clamp(0.0, (width - tabs[d.index.min(tabs.len() - 1)].w).max(0.0)));

        let draw_tab = |fonts: &mut FontSystem, batch: &mut Batch, i: usize, t: &T, tx: f32, active: bool, hovered: bool, close_hover: bool, lifted: bool| {
            if active || lifted {
                batch.rect(tx, 0.0, t.w, bar_h, theme.tab_active);
                batch.rect(tx, 0.0, t.w, 2.0 * s, theme.accent);
                batch.rect(tx + t.w - 1.0, 0.0, 1.0, bar_h, theme.highlight);
                batch.rect(tx - 1.0, 0.0, 1.0, bar_h, theme.highlight);
            } else {
                if hovered {
                    batch.rect(tx, 0.0, t.w, bar_h, theme.hover);
                }
                batch.rect(tx + t.w - 1.0, 0.0, 1.0, bar_h, theme.panel);
            }
            if lifted {
                batch.outline(tx, 0.0, t.w, bar_h, 0.0, 1.0, theme.accent);
            }
            let mut cx = tx + pad_x;
            let num_color = if active { theme.ansi[2] } else { theme.muted };
            cx += ui_text(fonts, batch, cx, text_y, &t.num, font, num_color, false);
            cx += gap;
            let fg = if active || hovered { theme.fg } else { scale_rgb(theme.fg, 0.75) };
            let renaming = rename.as_ref().filter(|(ri, _, _)| *ri == i);
            if let Some((_, _, all)) = renaming {
                // Inline edit: no box. Selected title is highlighted (typing
                // replaces it); otherwise just a caret after the text.
                let tw = ui_text_width(fonts, &t.label, font);
                // Real text only (trailing spaces included, so the caret
                // advances as you type them); padding is just width.
                let typed: String = t.label.chars().take(t.text_chars).collect();
                let typed_w = ui_text_width(fonts, &typed, font);
                if *all && !typed.is_empty() {
                    batch.rrect(cx - 2.0 * s, text_y - 1.0, typed_w + 4.0 * s, lh + 2.0, 3.0 * s, theme.selection);
                }
                ui_text(fonts, batch, cx, text_y, &typed, font, theme.fg, false);
                if !*all {
                    batch.rect(cx + typed_w + 1.0, text_y, 2.0 * s, lh, theme.cursor);
                }
                cx += tw;
            } else {
                cx += ui_text(fonts, batch, cx, text_y, &t.label, font, fg, false);
            }
            cx += gap;
            // Close button.
            if close_hover {
                let bw = adv + 8.0 * s;
                batch.rrect(cx - 4.0 * s, text_y - 3.0 * s, bw, lh + 6.0 * s, 4.0 * s, with_alpha(theme.ansi[1], 0.9));
                ui_text(fonts, batch, cx, text_y, "×", font, theme.fg, true);
            } else {
                let c = if active || hovered { theme.muted } else { with_alpha(theme.muted, 0.55) };
                ui_text(fonts, batch, cx, text_y, "×", font, c, false);
            }
            let _ = i;
        };

        for (i, t) in tabs.iter().enumerate() {
            let is_dragged = drag.map(|d| d.index == i).unwrap_or(false);
            let active = i == w.active;
            let hovered = matches!(w.hover, Hover::Tab(h) | Hover::Close(h) if h == i);
            let close_hover = w.hover == Hover::Close(i);
            if is_dragged {
                // Slot placeholder.
                batch.rect(t.x, 0.0, t.w, bar_h, with_alpha(theme.hover, 0.5));
            } else {
                draw_tab(fonts, batch, i, t, t.x, active, hovered, close_hover, false);
            }
            w.tab_hits.push(TabHit { index: i, x0: t.x, x1: t.x + t.w, close_x0: t.x + t.w - pad_x - close_w, close_x1: t.x + t.w });
        }

        // "+" button for a new tab.
        {
            let (x0, x1) = (tabs_end + 2.0 * s, tabs_end + 2.0 * s + plus_w);
            if w.hover == Hover::Plus {
                batch.rrect(x0 + 4.0 * s, 6.0 * s, plus_w - 8.0 * s, bar_h - 12.0 * s, 4.0 * s, theme.hover);
            }
            let fg = if w.hover == Hover::Plus { theme.fg } else { theme.muted };
            let pw = ui_text_width(fonts, "+", 14.0);
            ui_text(fonts, batch, x0 + (plus_w - pw) / 2.0, ((bar_h - fonts.line_height_for(14.0 * 1.12)) / 2.0).floor(), "+", 14.0, fg, false);
            w.plus_hit = Some((x0, x1));
            x = x1;
        }

        // Gear button (opens the Control Deck), then status to its left.
        let gear_used = w.deck.draw_gear(fonts, batch, theme, width, bar_h, s);
        let right_edge = width - gear_used;

        // Right-aligned status / scrollback indicator / drag hint.
        let mut right = String::new();
        if let Some(d) = drag {
            right = if d.outside {
                "release: own window · over another kindlyterm window: move there".into()
            } else {
                "drag to reorder · pull down to detach".into()
            };
        }
        if right.is_empty()
            && let Some((msg, at)) = &w.status
                && at.elapsed().as_secs() < 4 {
                    right = msg.clone();
                }
        if right.is_empty() && scroll_off > 0 {
            right = format!("↑ {scroll_off} lines  (Shift+End to return)");
        }
        if right.is_empty() {
            right = if n_shortcuts == 0 { "Ctrl+Shift+S saves a shortcut".into() } else { format!("{n_shortcuts} shortcuts · tap Ctrl+Shift") };
        }
        let rfont = 11.0;
        let rw = ui_text_width(fonts, &right, rfont);
        let rx = right_edge - 14.0 * s - rw;
        if rx > x + 12.0 * s {
            let ry = ((bar_h - fonts.line_height_for(rfont * 1.12)) / 2.0).floor();
            ui_text(fonts, batch, rx, ry, &right, rfont, theme.muted, false);
        }

        // The dragged tab on top, following the pointer.
        if let (Some(d), Some(gx)) = (drag, ghost_x)
            && let Some(t) = tabs.get(d.index) {
                let active = d.index == w.active;
                let lift_y = if d.outside { 6.0 * s } else { 0.0 };
                if lift_y > 0.0 {
                    batch.rect(gx, lift_y, t.w, bar_h, with_alpha(theme.tab_bar, 0.9));
                }
                draw_tab(fonts, batch, d.index, t, gx, active, true, false, true);
            }
    }

    /// Draw the active terminal at the window's grid origin (tabs mode).
    pub(super) fn draw_terminal(&mut self, l: Layout) {
        let theme = &self.theme;
        let anim_mode = self.config.terminal.cursor_animation.clone();
        let effects_cfg = self.effects.clone();
        if self.on_free_canvas() {
            self.draw_canvas(l);
            return;
        }
        let w = &mut self.wins[self.cur];
        let focused = w.focused;
        let Some(ti) = w.active_term_index() else { return };
        let place = match self.debug.term_zoom {
            // Debug: draw zoomed and clipped to a box in the top-left quarter.
            Some(z) => TermPlace { x: l.grid_x + 40.0, y: l.grid_y + 40.0, zoom: z, focused, clip: Some((l.grid_x + 20.0, l.grid_y + 20.0, 520.0, 320.0)) },
            None => TermPlace { x: l.grid_x, y: l.grid_y, zoom: 1.0, focused, clip: None },
        };
        if self.debug.term_zoom.is_some() {
            w.batch.outline(l.grid_x + 20.0, l.grid_y + 20.0, 520.0, 320.0, 6.0, 1.0, theme.accent);
        }
        let tab = &mut w.terms[ti];
        draw_term_view(&mut w.fonts, &mut w.batch, theme, &anim_mode, &effects_cfg, tab, place);
    }
}

/// Where and how large to draw a terminal.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TermPlace {
    /// Pixel origin of the grid's top-left corner.
    pub x: f32,
    pub y: f32,
    /// 1.0 = the window's base cell size. Text is rasterised at base × zoom.
    pub zoom: f32,
    pub focused: bool,
    /// Optional scissor rect (x, y, w, h) the drawing is clipped to.
    pub clip: Option<(f32, f32, f32, f32)>,
}

/// Draw one terminal's grid, effects and cursor. Works for any origin and
/// zoom so the same code serves the tabbed view and the canvas.
pub(crate) fn draw_term_view(
    fonts: &mut FontSystem,
    batch: &mut Batch,
    theme: &Theme,
    anim_mode: &str,
    effects_cfg: &EffectsConfig,
    tab: &mut Terminal,
    place: TermPlace,
) {
    {
        let tab_id = tab.id;
        let focused = place.focused;
        let zoom = place.zoom;
        let base_px = fonts.size_px;
        let rows = tab.size.rows;
        let anim = &mut tab.view.cursor_anim;
        let fx = &mut tab.view.fx;
        // Cell metrics scaled by zoom (base metrics stay in the atlas' units).
        let m0 = fonts.metrics;
        let m = crate::font::CellMetrics {
            width: m0.width * zoom,
            height: m0.height * zoom,
            ascent: m0.ascent * zoom,
            underline_pos: m0.underline_pos * zoom,
            underline_thickness: (m0.underline_thickness * zoom).max(1.0),
            strikeout_pos: m0.strikeout_pos * zoom,
        };
        if let Some((cx, cy, cw, ch)) = place.clip {
            batch.push_clip(cx, cy, cw, ch);
        }

        let term = tab.term.lock();
        let history_now = term.grid().history_size();
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let colors = content.colors;
        let selection = content.selection;
        let cursor = content.cursor;
        let cursor_vp = point_to_viewport(display_offset, cursor.point).filter(|p| p.line < rows);

        // --- cursor animation state -------------------------------------
        let now = Instant::now();
        let animate = anim_mode != "none";
        if let Some(vp) = cursor_vp {
            let target = (vp.column.0 as f32, vp.line as f32);
            if anim.tab != tab_id || !animate {
                // New tab: snap.
                anim.tab = tab_id;
                anim.from = target;
                anim.to = target;
                anim.pos = target;
                anim.move_start = now - std::time::Duration::from_secs(1);
            } else if anim.to != target {
                // Start a glide from wherever the cursor currently is drawn.
                anim.from = anim.pos;
                anim.to = target;
                anim.move_start = now;
            }
        }
        let travel_t = anim.travel_t();
        let ease = 1.0 - (1.0 - travel_t).powi(3);
        anim.pos = (anim.from.0 + (anim.to.0 - anim.from.0) * ease, anim.from.1 + (anim.to.1 - anim.from.1) * ease);
        // Cells -> pixels for this frame's placement.
        let cell_px = |c: (f32, f32)| (place.x + c.0 * m.width, place.y + c.1 * m.height);
        let travelling = animate && travel_t < 1.0;
        let idle_s = anim.last_input.elapsed().as_secs_f32();
        // Resting behaviour: breathe (soft alpha wave) or blink, after a pause.
        let (rest_alpha, rest_visible) = match anim_mode {
            "breathe" if idle_s > 0.7 => {
                let phase = ((idle_s - 0.7) / 2.4) * std::f32::consts::TAU;
                (0.62 + 0.38 * (0.5 + 0.5 * phase.cos()), true)
            }
            "blink" if idle_s > 0.5 => (1.0, (((idle_s - 0.5) * 1000.0 / 530.0) as u64).is_multiple_of(2)),
            _ => (1.0, true),
        };
        let pulse_t = if animate { anim.pulse_t() } else { None };
        // While gliding or pulsing, the block is drawn as an overlay after the
        // glyphs; at rest it inverts the cell like a classic block cursor.
        let block_inline = !travelling && pulse_t.is_none() && (!animate || anim.chomping().is_none());

        let resolve = |c: Color, bold: bool| -> Rgba {
            match c {
                Color::Spec(rgb) => rgb_to_rgba(rgb),
                Color::Named(n) => {
                    // Bold text uses the bright variant of the 8 base colors.
                    let n = if bold {
                        match n {
                            NamedColor::Black => NamedColor::BrightBlack,
                            NamedColor::Red => NamedColor::BrightRed,
                            NamedColor::Green => NamedColor::BrightGreen,
                            NamedColor::Yellow => NamedColor::BrightYellow,
                            NamedColor::Blue => NamedColor::BrightBlue,
                            NamedColor::Magenta => NamedColor::BrightMagenta,
                            NamedColor::Cyan => NamedColor::BrightCyan,
                            NamedColor::White => NamedColor::BrightWhite,
                            other => other,
                        }
                    } else {
                        n
                    };
                    if let Some(rgb) = colors[n] {
                        return rgb_to_rgba(rgb);
                    }
                    named_color(theme, n)
                }
                Color::Indexed(i) => {
                    if let Some(rgb) = colors[i as usize] {
                        return rgb_to_rgba(rgb);
                    }
                    let i = if bold && i < 8 { i + 8 } else { i };
                    indexed_color(theme, i)
                }
            }
        };
        let default_bg = colors[NamedColor::Background].map(rgb_to_rgba).unwrap_or(theme.bg);

        for cell in content.display_iter {
            let Some(vp) = point_to_viewport(display_offset, cell.point) else { continue };
            if vp.line >= rows {
                continue;
            }
            let flags = cell.flags;
            let x = place.x + vp.column.0 as f32 * m.width;
            let y = place.y + vp.line as f32 * m.height;
            let bold = flags.intersects(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);

            let mut fg = resolve(cell.fg, bold);
            let mut bg = resolve(cell.bg, false);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            let selected = selection.map(|s| s.contains(cell.point)).unwrap_or(false);
            if selected {
                bg = theme.selection;
            }
            let is_cursor = focused && block_inline && rest_visible && cursor_vp == Some(vp) && cursor.shape == CursorShape::Block;
            if is_cursor {
                bg = with_alpha(colors[NamedColor::Cursor].map(rgb_to_rgba).unwrap_or(theme.cursor), rest_alpha);
                fg = if rest_alpha > 0.8 { default_bg } else { fg };
            }

            let wide = flags.contains(Flags::WIDE_CHAR);
            let cell_w = if wide { m.width * 2.0 } else { m.width };
            if bg != default_bg {
                batch.rect(x, y, cell_w, m.height, bg);
            }

            if flags.contains(Flags::WIDE_CHAR_SPACER) || flags.contains(Flags::HIDDEN) {
                continue;
            }
            let c = cell.c;
            // Paste rain: newly pasted cells are hidden until their drop lands.
            if fx.watching() {
                fx.observe_cell(vp.column.0, vp.line, c, history_now);
            }
            if fx.is_masked(vp.column.0, vp.line) {
                continue;
            }
            if c != ' ' && c != '\t'
                && let Some(g) = fonts.glyph(GlyphKey::cell_zoomed(c, bold, italic, base_px, zoom)) {
                    batch.glyph(x, y + m.ascent, &g, fg);
                }
            if let Some(zw) = cell.zerowidth() {
                for &z in zw {
                    if let Some(g) = fonts.glyph(GlyphKey::cell_zoomed(z, bold, italic, base_px, zoom)) {
                        batch.glyph(x, y + m.ascent, &g, fg);
                    }
                }
            }
            if flags.intersects(Flags::ALL_UNDERLINES) {
                let ul = cell.underline_color().map(|c| resolve(c, false)).unwrap_or(fg);
                let t = m.underline_thickness;
                let uy = (y + m.underline_pos).min(y + m.height - t);
                if flags.contains(Flags::DOUBLE_UNDERLINE) {
                    batch.rect(x, uy - t, cell_w, t, ul);
                    batch.rect(x, uy + t, cell_w, t, ul);
                } else if flags.contains(Flags::DOTTED_UNDERLINE) || flags.contains(Flags::DASHED_UNDERLINE) {
                    let seg = if flags.contains(Flags::DOTTED_UNDERLINE) { t } else { (cell_w / 3.0).floor() };
                    let mut sx = x;
                    while sx < x + cell_w {
                        batch.rect(sx, uy, seg.min(x + cell_w - sx), t, ul);
                        sx += seg * 2.0;
                    }
                } else {
                    batch.rect(x, uy, cell_w, t, ul);
                }
            }
            if flags.contains(Flags::STRIKEOUT) {
                batch.rect(x, y + m.strikeout_pos, cell_w, m.underline_thickness, fg);
            }
        }

        // Visual effects (typing trail, paste rain) sit between grid and cursor.
        fx.draw(effects_cfg, fonts, batch, theme, place.x, place.y, zoom, now);

        // Cursor overlay: animated position, trail, pulse ring, other shapes.
        if let Some(vp) = cursor_vp {
            let _ = vp;
            let (x, y) = cell_px(anim.pos);
            let (fx0, fy0) = cell_px(anim.from);
            let color = colors[NamedColor::Cursor].map(rgb_to_rgba).unwrap_or(theme.cursor);
            let t = 2.0f32.max((m.width / 8.0).floor());
            let alpha = if focused { rest_alpha } else { 1.0 };

            // Motion trail from the previous cell.
            if travelling {
                let fade = (1.0 - travel_t) * 0.35;
                if (anim.from.1 - anim.to.1).abs() < 0.5 {
                    let x0 = fx0.min(x);
                    let x1 = fx0.max(x) + m.width;
                    batch.rrect(x0, y + 1.0, x1 - x0, m.height - 2.0, 3.0, with_alpha(color, fade));
                } else {
                    for k in 1..=4 {
                        let f = k as f32 / 5.0;
                        let gx = fx0 + (x - fx0) * f;
                        let gy = fy0 + (y - fy0) * f;
                        batch.rrect(gx, gy, m.width, m.height, 3.0, with_alpha(color, fade * f));
                    }
                }
            }

            // Focus / click pulse: a ring ripples outward and the block swells.
            let mut grow = 0.0;
            if let Some(pt) = pulse_t {
                let e = 1.0 - (1.0 - pt).powi(2);
                let spread = 16.0 * e;
                batch.outline(x - spread, y - spread, m.width + 2.0 * spread, m.height + 2.0 * spread, 4.0 + spread * 0.5, 2.0, with_alpha(color, (1.0 - pt) * 0.9));
                if pt > 0.12 {
                    let e2 = 1.0 - (1.0 - (pt - 0.12) / 0.88).powi(2);
                    let spread2 = 10.0 * e2;
                    batch.outline(x - spread2, y - spread2, m.width + 2.0 * spread2, m.height + 2.0 * spread2, 3.0 + spread2 * 0.5, 1.0, with_alpha(color, (1.0 - pt) * 0.5));
                }
                grow = 3.0 * (1.0 - pt);
            }

            // Pac-Man: replaces the cursor while Backspace/Delete is held.
            let chomping = if animate { anim.chomping() } else { None };
            if anim.chomp.map(|(_, last, _)| last.elapsed().as_millis() > 400).unwrap_or(false) {
                anim.chomp = None;
            }
            if let Some(left) = chomping {
                let size = m.height * 1.05;
                let cx = x + m.width / 2.0 - size / 2.0;
                let cy = y + m.height / 2.0 - size / 2.0;
                let tt = anim.chomp.map(|(s, _, _)| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                let mouth = 0.08 + 0.55 * (tt * 16.0).sin().abs();
                let facing = if left { std::f32::consts::PI } else { 0.0 };
                let yellow = rgb([0xff, 0xe1, 0x35]);
                batch.pacman(cx, cy, size, facing, mouth, yellow);
                // Eye: a dark dot up and toward the mouth.
                let eye = (size * 0.13).max(2.0);
                let ex = if left { cx + size * 0.30 } else { cx + size * 0.57 };
                batch.rrect(ex, cy + size * 0.22, eye, eye, eye / 2.0, theme.bg);
                // Crumbs behind it.
                for k in 1..=3 {
                    let dx = if left { m.width * k as f32 } else { -m.width * k as f32 };
                    let phase = ((tt * 16.0 + k as f32) % 2.0) / 2.0;
                    let crumb = (2.0_f32 + 2.0 * phase).min(3.5);
                    batch.rrect(x + m.width / 2.0 + dx - crumb / 2.0, y + m.height / 2.0 - crumb / 2.0, crumb, crumb, crumb / 2.0, with_alpha(yellow, 0.5 * (1.0 - phase)));
                }
            } else {
            match (cursor.shape, focused) {
                (CursorShape::Hidden, _) => {}
                (CursorShape::Block, true) => {
                    if !block_inline && rest_visible {
                        batch.rrect(x - grow, y - grow, m.width + 2.0 * grow, m.height + 2.0 * grow, 2.0 + grow, with_alpha(color, 0.85 * alpha));
                    }
                }
                (CursorShape::Block, false) | (CursorShape::HollowBlock, _) => {
                    batch.outline(x, y, m.width, m.height, 2.0, 1.0, color);
                }
                (CursorShape::Beam, _) => {
                    if rest_visible {
                        batch.rrect(x - grow * 0.5, y - grow, t + grow, m.height + 2.0 * grow, 1.5, with_alpha(color, alpha));
                    }
                }
                (CursorShape::Underline, _) => {
                    if rest_visible {
                        batch.rrect(x - grow, y + m.height - t - grow, m.width + 2.0 * grow, t + grow, 1.5, with_alpha(color, alpha));
                    }
                }
            }
            }
        }
        drop(term);
        if place.clip.is_some() {
            batch.pop_clip();
        }
    }
}

impl App {
    pub(super) fn draw_palette(&mut self, l: Layout) {
        let theme = &self.theme;
        let win = &mut self.wins[self.cur];
        let Some(p) = win.palette.as_ref() else { return };
        let fonts = &mut win.fonts;
        let batch = &mut win.batch;
        let (w, h) = (win.renderer.width as f32, win.renderer.height as f32);
        let (cw, ch) = (l.cell_w, l.cell_h);

        // Dim everything behind.
        batch.rect(0.0, 0.0, w, h, [0.0, 0.0, 0.0, 0.45]);

        let box_cols = l.cols.saturating_sub(4).clamp(20, 90);
        let list_rows = if p.is_list() { p.filtered.len().clamp(1, 12) } else { 0 };
        let box_rows = 1 + 1 + list_rows + 1 + if p.is_list() { 1 } else { 0 }; // title, input, list, gap/hint
        let bw = box_cols as f32 * cw + 2.0 * cw;
        let bh = box_rows as f32 * ch + ch;
        let bx = ((w - bw) / 2.0).floor();
        let by = (l.grid_y + ch).floor();

        batch.rect(bx - 1.0, by - 1.0, bw + 2.0, bh + 2.0, theme.accent);
        batch.rect(bx, by, bw, bh, theme.panel);

        let tx = bx + cw;
        let mut ty = by + ch * 0.5;
        Self::text(fonts, batch, tx, ty, &p.title(), theme.accent, true, box_cols);
        ty += ch;

        // Input line with a block cursor.
        let prompt = "> ";
        Self::text(fonts, batch, tx, ty, prompt, theme.accent, true, 2);
        let shown: String = {
            let n = p.input.chars().count();
            let max = box_cols.saturating_sub(3);
            if n > max { p.input.chars().skip(n - max).collect() } else { p.input.clone() }
        };
        let used = Self::text(fonts, batch, tx + 2.0 * cw, ty, &shown, theme.fg, false, box_cols - 2);
        batch.rect(tx + 2.0 * cw + used, ty, cw, ch, with_alpha(theme.cursor, 0.8));
        ty += ch;

        if p.is_list() {
            // Keep the selection visible in the window of rows.
            let start = p.selected.saturating_sub(list_rows.saturating_sub(1));
            let start = start.min(p.filtered.len().saturating_sub(list_rows));
            if p.filtered.is_empty() {
                Self::text(fonts, batch, tx, ty, "no matches", theme.muted, false, box_cols);
                ty += ch;
            }
            for (row, &idx) in p.filtered.iter().enumerate().skip(start).take(list_rows) {
                let item = &p.items[idx];
                let selected = row == p.selected;
                if selected {
                    batch.rect(bx + cw * 0.5, ty, bw - cw, ch, theme.highlight);
                    batch.rect(bx + cw * 0.5, ty, 3.0, ch, theme.accent);
                }
                let label_cells = box_cols.min(28);
                Self::text(fonts, batch, tx, ty, &item.label, theme.fg, selected, label_cells);
                let detail_x = tx + (label_cells as f32 + 1.0) * cw;
                let detail_cells = box_cols.saturating_sub(label_cells + 1);
                Self::text(fonts, batch, detail_x, ty, &item.detail, theme.muted, false, detail_cells);
                ty += ch;
            }
            ty += ch * 0.25;
        }
        Self::text(fonts, batch, tx, ty, p.hint(), theme.muted, false, box_cols);
    }
}

impl App {
    /// Keyboard cheat sheet: kindlyterm keys on the left, readline on the right.
    pub(super) fn draw_cheat(&mut self, l: Layout) {
        let theme = &self.theme;
        let win = &mut self.wins[self.cur];
        if !win.cheat {
            return;
        }
        let fonts = &mut win.fonts;
        let batch = &mut win.batch;
        let s = win.scale as f32;
        let (w, h) = (win.renderer.width as f32, win.renderer.height as f32);
        batch.rect(0.0, 0.0, w, h, [0.0, 0.0, 0.0, 0.5]);

        type Sec<'a> = (&'a str, &'a [(&'a str, &'a str)]);
        let left: &[Sec] = &[
            ("CONTROL DECK", &[
                ("Ctrl+Shift (tap)  ·  Ctrl+Shift+,", "toggle the Deck"),
                ("Ctrl+Shift+Space", "quick-run a shortcut"),
                ("Ctrl+Shift+S", "save selection / new shortcut"),
                ("Alt+…  (your hotkeys)", "launch a shortcut"),
            ]),
            ("TABS & WINDOWS", &[
                ("Ctrl+Shift+T  /  Ctrl+Shift+W", "new / close tab"),
                ("Ctrl+Tab  ·  Alt+1…9", "switch tabs"),
                ("Ctrl+Shift+N", "new window"),
                ("double-click title", "rename tab"),
                ("drag tab", "reorder · pull down to detach"),
                ("drop on another window", "move tab there"),
            ]),
            ("TERMINAL", &[
                ("Shift+PageUp / PageDown", "scroll history"),
                ("Shift+Home / End", "top / bottom"),
                ("Ctrl+Shift+C / V  ·  Shift+Insert", "copy / paste"),
                ("Ctrl+C with selection", "copy"),
                ("Ctrl+V at a prompt", "paste"),
                ("Ctrl+=  /  Ctrl+−  /  Ctrl+0", "font size"),
                ("Ctrl+/", "this cheat sheet"),
            ]),
        ];
        let right: &[Sec] = &[
            ("SHELL LINE EDITING  (readline, emacs mode)", &[
                ("Ctrl+A  /  Ctrl+E", "start / end of line"),
                ("Alt+B  /  Alt+F", "back / forward a word"),
                ("Ctrl+W  /  Alt+D", "delete word left / right"),
                ("Ctrl+U  /  Ctrl+K", "delete to start / to end"),
                ("Ctrl+Y", "paste what you deleted"),
                ("Ctrl+_  (Ctrl+Shift+-)", "undo (a paste undoes at once)"),
                ("Ctrl+R", "search history"),
                ("Alt+.", "last arg of previous command"),
                ("Ctrl+L", "clear screen"),
                ("Ctrl+Z  /  fg", "suspend program / resume"),
            ]),
            ("CANVAS", &[
                ("Ctrl+Shift+Enter  /  K", "add a terminal · new canvas tab"),
                ("drag title  ·  drag edge", "move · resize"),
                ("wheel  ·  Ctrl+wheel  ·  Space+drag", "pan · zoom · pan"),
                ("Ctrl+Shift+F  ·  Ctrl+Shift+A", "focus mode · fit all"),
                ("Ctrl+Shift+=  /  −  /  0", "zoom in / out / reset"),
            ]),
            ("PREFER VIM KEYS?", &[
                ("~/.inputrc", "set editing-mode vi"),
                ("then Esc", "normal mode: u · dd · cw · 0 · $"),
                ("paste highlight off", "set enable-active-region off"),
            ]),
        ];

        let fs_key = 11.0;
        let fs_val = 11.0;
        let fs_head = 10.0;
        let row_h = (fonts.line_height_for(fs_key * 1.12) + 6.0 * s).round();
        let head_h = row_h + 8.0 * s;
        let col_w = (w * 0.5 - 50.0 * s).min(560.0 * s).max(300.0 * s);
        let key_w = (col_w * 0.56).round();
        let gap_h = 12.0 * s;
        let count = |secs: &[Sec]| secs.iter().map(|(_, rows)| rows.len() as f32 * row_h + head_h + gap_h).sum::<f32>() - gap_h;
        let content_h = count(left).max(count(right));
        let pad = 22.0 * s;
        let box_w = col_w * 2.0 + pad * 3.0;
        let box_h = content_h + pad * 2.0 + 30.0 * s;
        let bx = ((w - box_w) / 2.0).max(8.0).floor();
        let by = ((h - box_h) / 2.0).max(l.tab_bar_h + 8.0).floor();
        batch.rrect(bx, by, box_w, box_h, 8.0 * s, theme.panel);
        batch.outline(bx, by, box_w, box_h, 8.0 * s, 1.0, theme.accent);

        let draw_col = |fonts: &mut FontSystem, batch: &mut Batch, x: f32, secs: &[Sec]| {
            let mut y = by + pad;
            for (i, (title, rows)) in secs.iter().enumerate() {
                if i > 0 {
                    y += gap_h;
                }
                ui_text_tracked_pub(fonts, batch, x, y + 2.0 * s, title, fs_head, theme.accent);
                y += head_h;
                for (k, v) in rows.iter() {
                    ui_text(fonts, batch, x, y, k, fs_key, theme.fg, false);
                    ui_text(fonts, batch, x + key_w, y, v, fs_val, theme.muted, false);
                    y += row_h;
                }
            }
        };
        draw_col(fonts, batch, bx + pad, left);
        draw_col(fonts, batch, bx + pad * 2.0 + col_w, right);
        // Divider and footer.
        batch.rect(bx + pad * 1.5 + col_w, by + pad, 1.0, content_h, theme.highlight);
        let foot = "any key or click closes  ·  full list in README.md";
        let fw = ui_text_width(fonts, foot, 10.0);
        ui_text(fonts, batch, bx + (box_w - fw) / 2.0, by + box_h - pad - 4.0 * s, foot, 10.0, theme.muted, false);
    }

    pub(super) fn draw_deck(&mut self, l: Layout) {
        let win = &mut self.wins[self.cur];
        if !win.deck.visible() {
            return;
        }
        let (w, h) = (win.renderer.width as f32, win.renderer.height as f32);
        let family = win.fonts.family.clone();
        let env = DeckEnv {
            config: &self.config,
            shortcuts: &self.store.commands,
            state: &self.deck_state,
            theme: &self.theme,
            font_families: &self.font_families,
            font_family: &family,
            tab_count: win.terms.len(),
            gpu: &self.gpu_name,
            scale: win.scale as f32,
            effects: &self.effects,
        };
        win.deck.draw(&mut win.fonts, &mut win.batch, &env, w, h, l.tab_bar_h);
    }

    pub(super) fn draw_menu(&mut self, l: Layout) {
        let theme = &self.theme;
        let win = &mut self.wins[self.cur];
        let Some(menu) = win.menu.as_mut() else { return };
        let fonts = &mut win.fonts;
        let batch = &mut win.batch;
        let (w, h) = (win.renderer.width as f32, win.renderer.height as f32);
        let (cw, ch) = (l.cell_w, l.cell_h);
        let row_h = (ch * 1.3).round();
        let width = menu.width_cells() as f32 * cw;
        let height = menu.items.len() as f32 * row_h + row_h * 0.5;
        // Keep the menu on screen.
        let x = menu.x.min(w - width - 2.0).max(0.0).floor();
        let y = menu.y.min(h - height - 2.0).max(0.0).floor();
        menu.rect = (x, y, width, height);
        menu.row_h = row_h;

        batch.rect(x - 1.0, y - 1.0, width + 2.0, height + 2.0, theme.accent);
        batch.rect(x, y, width, height, theme.panel);

        // Every item, separators included, takes one row_h slot so that
        // Menu::hit can map a pixel to an index arithmetically.
        let mut ry = y + row_h * 0.25;
        for (i, item) in menu.items.iter().enumerate() {
            if item.action == MenuAction::Separator {
                batch.rect(x + cw, ry + row_h * 0.5 - 0.5, width - 2.0 * cw, 1.0, with_alpha(theme.muted, 0.5));
                ry += row_h;
                continue;
            }
            let selected = menu.selected == Some(i);
            if selected {
                batch.rect(x + 2.0, ry, width - 4.0, row_h, theme.highlight);
                batch.rect(x + 2.0, ry, 3.0, row_h, theme.accent);
            }
            let fg = if !item.enabled { with_alpha(theme.muted, 0.6) } else { theme.fg };
            let ty = ry + ((row_h - ch) / 2.0).floor();
            Self::text(fonts, batch, x + 2.0 * cw, ty, &item.label, fg, false, 60);
            if !item.shortcut.is_empty() {
                let sw = item.shortcut.len() as f32 * cw;
                Self::text(fonts, batch, x + width - sw - 2.0 * cw, ty, item.shortcut, with_alpha(theme.muted, 0.9), false, 30);
            }
            ry += row_h;
        }
    }
}
