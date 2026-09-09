//! Pinned items (screen-space, on top, immune to pan and zoom) and mirrors
//! (a second live view of a terminal on the same canvas).

use super::*;
use crate::canvas::Corner;

impl App {
    // -----------------------------------------------------------------------
    // Pins
    // -----------------------------------------------------------------------

    /// Screen rect of a pinned item: its rect is area-relative pixels.
    pub(super) fn pinned_screen_rect(l: Layout, item: &Item) -> WRect {
        WRect::new(l.area.x + item.rect.x, l.area.y + item.rect.y, item.rect.w, item.rect.h)
    }

    /// Pointer position in an item's own coordinate space: world units for
    /// a canvas item, area-relative pixels for a pinned one.
    pub(super) fn item_point(&self, id: ItemId, mx: f32, my: f32) -> (f32, f32) {
        let w = self.win();
        let Some(l) = w.layout else { return (mx, my) };
        let Some(c) = w.canvas() else { return (mx, my) };
        match c.item(id).map(|i| i.pin.is_some()) {
            Some(true) => (mx - l.area.x, my - l.area.y),
            _ => c.view.screen_to_world(l.area, mx, my),
        }
    }

    pub(super) fn is_pinned(&self, id: ItemId) -> bool {
        self.win().canvas().and_then(|c| c.item(id)).map(|i| i.pin.is_some()).unwrap_or(false)
    }

    fn nearest_corner(area: WRect, r: WRect) -> Corner {
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        match (cx < area.w / 2.0, cy < area.h / 2.0) {
            (true, true) => Corner::TopLeft,
            (false, true) => Corner::TopRight,
            (true, false) => Corner::BottomLeft,
            (false, false) => Corner::BottomRight,
        }
    }

    /// Keep a pinned item inside the area and re-anchor it to the nearest
    /// corner (call after a move or resize).
    pub(super) fn settle_pin(&mut self, id: ItemId) {
        let Some(l) = self.win().layout else { return };
        let w = self.win_mut();
        let Some(it) = w.canvas_mut().and_then(|c| c.item_mut(id)) else { return };
        if it.pin.is_none() {
            return;
        }
        let max_x = (l.area.w - it.rect.w).max(0.0);
        let max_y = (l.area.h - it.rect.h).max(0.0);
        it.rect.x = it.rect.x.clamp(0.0, max_x).round();
        it.rect.y = it.rect.y.clamp(0.0, max_y).round();
        it.pin = Some(Self::nearest_corner(l.area, it.rect));
    }

    /// Ctrl+Shift+P / menu: pin the focused item to the screen, or unpin it
    /// back onto the canvas where it currently appears.
    pub(super) fn toggle_pin(&mut self, id: ItemId) {
        if !self.on_free_canvas() {
            self.set_status("Pins live on canvas tabs (Ctrl+Shift+Enter turns this one into a canvas)".into());
            return;
        }
        let Some(l) = self.win().layout else { return };
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        let view = c.view;
        let Some(it) = c.item_mut(id) else { return };
        let msg = if it.pin.is_some() {
            // Back to world coordinates at the same screen position.
            let (wx, wy) = view.screen_to_world(l.area, l.area.x + it.rect.x, l.area.y + it.rect.y);
            it.rect.x = wx.round();
            it.rect.y = wy.round();
            it.pin = None;
            "unpinned"
        } else {
            // Same size at 1:1; positioned where it is on screen now.
            let sr = view.rect_to_screen(l.area, it.rect);
            it.rect.x = sr.x - l.area.x;
            it.rect.y = sr.y - l.area.y;
            it.pin = Some(Corner::TopLeft);
            for g in &mut c.groups {
                g.members.retain(|m| *m != id);
            }
            "pinned to the screen · Ctrl+Shift+P again to release"
        };
        c.raise(id);
        c.selected.retain(|s| *s != id);
        w.dirty = true;
        self.settle_pin(id);
        self.set_status(msg.into());
        self.relayout();
    }

    /// The window changed size: pinned items keep their distance from the
    /// corner they are anchored to.
    pub(super) fn reanchor_pins(w: &mut Win, old: WRect, new: WRect) {
        let (dw, dh) = (new.w - old.w, new.h - old.h);
        if dw.abs() < 0.5 && dh.abs() < 0.5 {
            return;
        }
        for c in &mut w.canvases {
            for it in &mut c.items {
                if let Some(corner) = it.pin {
                    if matches!(corner, Corner::TopRight | Corner::BottomRight) {
                        it.rect.x += dw;
                    }
                    if matches!(corner, Corner::BottomLeft | Corner::BottomRight) {
                        it.rect.y += dh;
                    }
                    it.rect.x = it.rect.x.clamp(0.0, (new.w - it.rect.w).max(0.0));
                    it.rect.y = it.rect.y.clamp(0.0, (new.h - it.rect.h).max(0.0));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mirrors
    // -----------------------------------------------------------------------

    /// Add a second view of item `id`'s terminal beside it.
    pub(super) fn mirror_item(&mut self, id: ItemId) {
        let Some(l) = self.win().layout else { return };
        let item_id = self.next_item_id;
        self.next_item_id += 1;
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        let Some(src) = c.item(id).cloned() else { return };
        let ItemKind::Terminal(tab) = src.kind else { return };
        let rect = c.spawn_rect(l.area, src.rect.w, src.rect.h);
        c.items.push(Item { id: item_id, kind: ItemKind::Terminal(tab), rect, name: None, launch: src.launch.clone(), pin: None, mirror: true, monitor: None });
        c.focus = Some(item_id);
        c.selected.clear();
        w.dirty = true;
        self.reveal_rect(rect);
        self.set_status("mirror added · both views are live, either can type".into());
        self.request_redraw();
    }

    /// Close by item: a mirror just disappears; an owner ends the terminal
    /// (and every mirror of it).
    pub(super) fn close_item(&mut self, id: ItemId, event_loop: &ActiveEventLoop) {
        let info = self.win().canvas().and_then(|c| c.item(id)).map(|i| (i.mirror, i.kind.clone()));
        match info {
            Some((_, ItemKind::Image { .. })) => {
                let w = self.win_mut();
                Self::drop_image(w, id);
                if let Some(c) = w.canvas_mut() {
                    c.items.retain(|i| i.id != id);
                    if c.focus == Some(id) {
                        c.focus = c.items.last().map(|i| i.id);
                    }
                    c.selected.retain(|s| *s != id);
                }
                w.dirty = true;
                self.request_redraw();
            }
            Some((true, _)) => {
                let w = self.win_mut();
                if let Some(c) = w.canvas_mut() {
                    c.items.retain(|i| i.id != id);
                    if c.focus == Some(id) {
                        c.focus = c.items.last().map(|i| i.id);
                    }
                    c.selected.retain(|s| *s != id);
                }
                w.dirty = true;
                self.update_window_title();
                self.request_redraw();
            }
            Some((false, ItemKind::Terminal(t))) => self.close_terminal(t, event_loop),
            _ => {}
        }
    }

    /// All items showing a terminal share one grid: after one of them is
    /// resized, give the others the same frame size.
    pub(super) fn sync_mirror_sizes(&mut self, id: ItemId) {
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        let Some(src) = c.item(id) else { return };
        let ItemKind::Terminal(tab) = src.kind else { return };
        let (sw, sh) = (src.rect.w, src.rect.h);
        for it in c.items.iter_mut() {
            if it.id != id && matches!(it.kind, ItemKind::Terminal(t) if t == tab) {
                it.rect.w = sw;
                it.rect.h = sh;
            }
        }
    }
}
