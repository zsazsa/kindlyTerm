//! The dock: one terminal shown in a corner of every screen of its window,
//! on top of whatever tab is active, so the card you talk to (say, a
//! dispatching Claude) stays in view while you jump around. Click it to go
//! to its card, scroll it with the wheel, right-click to stop.

use super::draw::{TermPlace, draw_term_view};
use super::*;
use crate::canvas::{Corner, ITEM_PAD, TITLE_H};

/// A docked terminal.
#[derive(Clone, Copy, Debug)]
pub(super) struct Dock {
    pub tab: TabId,
    pub corner: Corner,
}

/// Gap between the dock and the edges of the area.
const DOCK_MARGIN: f32 = 12.0;

impl App {
    /// Dock the terminal behind item `id`, or undock it if it is the dock.
    pub(super) fn toggle_dock(&mut self, id: ItemId) {
        if let Some(tab) = self.tab_of_item_pub(id) {
            self.toggle_dock_tab(tab);
        }
    }

    pub(super) fn toggle_dock_tab(&mut self, tab: TabId) {
        let was = self.is_docked(tab);
        let title = self.win().term(tab).map(|t| t.display_title().to_string()).unwrap_or_default();
        let w = self.win_mut();
        w.dock = if was { None } else { Some(Dock { tab, corner: Corner::TopRight }) };
        w.dock_rect = None;
        w.dirty = true;
        if was {
            self.set_status(format!("{title} unpinned from every screen"));
        } else {
            self.set_status(format!("{title} pinned to every screen, top right (right-click it to unpin)"));
        }
        self.request_redraw();
    }

    pub(super) fn is_docked(&self, tab: TabId) -> bool {
        self.win().dock.map(|d| d.tab == tab).unwrap_or(false)
    }

    /// The dock, if the pointer is over it.
    pub(super) fn dock_hit(&self, mx: f32, my: f32) -> Option<Dock> {
        let w = self.win();
        let d = w.dock?;
        w.dock_rect.filter(|r| r.contains(mx, my)).map(|_| d)
    }

    /// Draw the dock over the active tab, unless that tab already shows its
    /// terminal (then it would be there twice).
    pub(super) fn draw_dock(&mut self, l: Layout) {
        let anim_mode = self.config.terminal.cursor_animation.clone();
        let chomp = self.config.terminal.chomp.clone();
        let effects_cfg = self.effects.clone();
        let opacity = self.config.colors.opacity.clamp(0.3, 1.0);
        let theme = &self.theme;
        let w = &mut self.wins[self.cur];
        w.dock_rect = None;
        let Some(dock) = w.dock else { return };
        let Some(ti) = w.term_index(dock.tab) else {
            w.dock = None;
            return;
        };
        let on_screen = w.canvas().map(|c| c.items.iter().any(|i| matches!(i.kind, ItemKind::Terminal(t) if t == dock.tab))).unwrap_or(false);
        if on_screen {
            return;
        }
        let area = l.area;
        let m = w.fonts.metrics;
        let grid_w = w.terms[ti].size.cols as f32 * m.width;
        let grid_h = w.terms[ti].size.rows as f32 * m.height;
        // As large as the terminal wants, capped at roughly half the area.
        let want_w = grid_w + 2.0 * ITEM_PAD + 2.0;
        let want_h = grid_h + TITLE_H + 2.0 * ITEM_PAD + 1.0;
        let dw = want_w.min((area.w * 0.45).max(320.0)).min(area.w - 2.0 * DOCK_MARGIN).max(120.0);
        let dh = want_h.min((area.h * 0.5).max(200.0)).min(area.h - 2.0 * DOCK_MARGIN).max(TITLE_H + 40.0);
        let (dx, dy) = match dock.corner {
            Corner::TopLeft => (area.x + DOCK_MARGIN, area.y + DOCK_MARGIN),
            Corner::TopRight => (area.right() - dw - DOCK_MARGIN, area.y + DOCK_MARGIN),
            Corner::BottomLeft => (area.x + DOCK_MARGIN, area.bottom() - dh - DOCK_MARGIN),
            Corner::BottomRight => (area.right() - dw - DOCK_MARGIN, area.bottom() - dh - DOCK_MARGIN),
        };
        let sr = WRect::new(dx.round(), dy.round(), dw.round(), dh.round());
        w.dock_rect = Some(sr);

        // Shadow, frame, title bar, accent outline: the pinned-card look.
        let radius = 8.0;
        let title_h = TITLE_H;
        w.batch.rrect(sr.x + 2.0, sr.y + 3.0, sr.w, sr.h, radius, with_alpha(theme.bg, 0.55));
        w.batch.rrect(sr.x, sr.y, sr.w, sr.h, radius, with_alpha(theme.bg, opacity.max(0.92)));
        w.batch.rrect(sr.x, sr.y, sr.w, title_h, radius, theme.tab_bar);
        w.batch.rect(sr.x, sr.y + title_h - radius, sr.w, radius, theme.tab_bar);
        w.batch.outline(sr.x, sr.y, sr.w, sr.h, radius, 1.0, theme.accent);

        let fs = 12.0;
        let lh = w.fonts.line_height_for(fs * 1.12);
        let ty = sr.y + (title_h - lh) / 2.0;
        let pad = 10.0;
        let hint = "click: go there · right-click: unpin";
        let hint_fs = 10.0;
        let hint_w = ui_text_width(&w.fonts, hint, hint_fs);
        let title = format!("⧉ {}", w.terms[ti].display_title());
        let adv = w.fonts.advance_for(fs * 1.12);
        let max_chars = ((sr.w - pad * 2.0 - hint_w - 12.0) / adv).floor().max(4.0) as usize;
        let shown: String = if title.chars().count() > max_chars {
            title.chars().take(max_chars.saturating_sub(1)).chain(std::iter::once('…')).collect()
        } else {
            title
        };
        ui_text(&mut w.fonts, &mut w.batch, sr.x + pad, ty, &shown, fs, theme.fg, true);
        let hy = sr.y + (title_h - w.fonts.line_height_for(hint_fs * 1.12)) / 2.0;
        ui_text(&mut w.fonts, &mut w.batch, sr.right() - pad - hint_w, hy, hint, hint_fs, theme.muted, false);

        // Content at zoom 1. When the grid is taller than the dock, its
        // bottom (the prompt, the latest lines) is what stays visible.
        let content = WRect::new(sr.x + 1.0, sr.y + title_h, sr.w - 2.0, sr.h - title_h - 1.0);
        let cx = content.x + ITEM_PAD;
        let cy = if grid_h + 2.0 * ITEM_PAD > content.h { content.bottom() - ITEM_PAD - grid_h } else { content.y + ITEM_PAD };
        let clip = (content.x, content.y, content.w, content.h);
        let place = TermPlace { x: cx, y: cy, zoom: 1.0, glyph_zoom: 1.0, focused: false, clip: Some(clip) };
        let tab = &mut w.terms[ti];
        draw_term_view(&mut w.fonts, &mut w.batch, theme, &anim_mode, &chomp, &effects_cfg, tab, place);
    }
}
