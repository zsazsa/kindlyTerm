//! Free-canvas presentation and interaction: drawing items, pointer handling (pan, zoom, move, resize), focus mode, canvas
//! management, and layout persistence.

use super::*;
use super::draw::{draw_term_view, TermPlace};
use crate::canvas::{item_part, Edge, GroupPart, Viewport, EDGE_PX, MAX_ZOOM, MIN_ZOOM, SNAP_PX};
use winit::dpi::PhysicalSize;

/// Minimum item grid while resizing.
const MIN_COLS: usize = 10;
const MIN_ROWS: usize = 3;

impl App {
    // -----------------------------------------------------------------------
    // Geometry helpers
    // -----------------------------------------------------------------------

    /// Screen rect of an item on the active canvas.
    pub(super) fn item_screen_rect(w: &Win, l: Layout, item: &Item) -> WRect {
        if item.pin.is_some() {
            return Self::pinned_screen_rect(l, item);
        }
        w.canvas().map(|c| c.view.rect_to_screen(l.area, item.rect)).unwrap_or(item.rect)
    }

    /// Zoom an item is drawn at: the view's, or 1 for a pinned item.
    pub(super) fn item_zoom(c: &Canvas, item: &Item) -> f32 {
        if item.pin.is_some() { 1.0 } else { c.view.zoom }
    }

    /// Is the active tab a free canvas?
    pub(super) fn on_free_canvas(&self) -> bool {
        self.win().canvas().map(|c| !c.is_single()).unwrap_or(false)
    }

    /// Item and part under a screen point (topmost first).
    pub(super) fn item_at(&self, sx: f32, sy: f32) -> Option<(ItemId, ItemPart)> {
        let w = self.win();
        let l = w.layout?;
        if !l.area.contains(sx, sy) {
            return None;
        }
        let c = w.canvas()?;
        // Pinned items sit on top of everything.
        let order = c.items.iter().rev().filter(|i| i.pin.is_some()).chain(c.items.iter().rev().filter(|i| i.pin.is_none()));
        for it in order {
            let zoom = Self::item_zoom(c, it);
            let close_w = (TITLE_H * zoom).max(12.0);
            let sr = Self::item_screen_rect(w, l, it);
            if let Some(part) = item_part(sr, TITLE_H * zoom, sx, sy, close_w) {
                return Some((it.id, part));
            }
        }
        None
    }

    /// Screen origin and zoom of the focused terminal's grid, for mouse
    /// selection; None if the focus is not a terminal.
    pub(super) fn focused_grid_origin(&self) -> Option<(f32, f32, f32)> {
        let w = self.win();
        let l = w.layout?;
        let c = w.canvas()?;
        if c.is_single() {
            return Some((l.grid_x, l.grid_y, 1.0));
        }
        let item = c.item(c.focus?)?;
        let sr = Self::item_screen_rect(w, l, item);
        let z = Self::item_zoom(c, item);
        Some((sr.x + ITEM_PAD * z, sr.y + (TITLE_H + ITEM_PAD) * z, z))
    }

    // -----------------------------------------------------------------------
    // Canvas management
    // -----------------------------------------------------------------------

    fn mark_dirty(&mut self) {
        self.win_mut().dirty = true;
    }

    /// Move a terminal item from the active canvas to tab `to`.
    pub(super) fn move_item_to_canvas(&mut self, tab: TabId, to: usize) {
        let l = self.win().layout.expect("layout");
        let w = self.win_mut();
        let from = w.active;
        if to >= w.canvases.len() || to == from {
            return;
        }
        if w.canvases[to].is_single() {
            // Target becomes a free canvas holding both.
            let g = Self::grid_size_of(w);
            let (iw, ih) = w.rect_for_grid(g.cols.min(120), g.rows.min(40));
            let c = &mut w.canvases[to];
            c.mode = CanvasMode::Free;
            if let Some(it) = c.items.first_mut() {
                it.rect = WRect::new(0.0, 0.0, iw, ih);
            }
            c.view = crate::canvas::Viewport { x: -((l.area.w - iw) / 2.0).round(), y: -((l.area.h - ih) / 2.0).round(), zoom: 1.0 };
        }
        // A terminal and its mirrors travel together.
        let ids = w.canvases[from].items_for_tab(tab);
        if ids.is_empty() {
            return;
        }
        let mut moved: Vec<Item> = Vec::new();
        for id in &ids {
            if let Some(pos) = w.canvases[from].items.iter().position(|i| i.id == *id) {
                moved.push(w.canvases[from].items.remove(pos));
            }
        }
        for g in &mut w.canvases[from].groups {
            g.members.retain(|m| !ids.contains(m));
        }
        w.canvases[from].selected.retain(|s| !ids.contains(s));
        if w.canvases[from].focus.map(|f| ids.contains(&f)).unwrap_or(false) {
            w.canvases[from].focus = w.canvases[from].items.last().map(|i| i.id);
        }
        let mut last = None;
        for mut item in moved {
            item.pin = None;
            let r = w.canvases[to].spawn_rect(l.area, item.rect.w, item.rect.h);
            item.rect = r;
            last = Some(item.id);
            w.canvases[to].items.push(item);
        }
        w.canvases[to].focus = last;
        w.dirty = true;
        let name = w.tab_title(to);
        // A single tab that lost its only terminal closes.
        if w.canvases[from].items.is_empty() && w.canvases[from].is_single() {
            w.canvases.remove(from);
            if w.active >= w.canvases.len() {
                w.active = w.canvases.len() - 1;
            }
        }
        self.relayout();
        self.set_status(format!("moved to {name}"));
    }

    // -----------------------------------------------------------------------
    // Viewport actions
    // -----------------------------------------------------------------------

    pub(super) fn zoom_by(&mut self, factor: f32, about: Option<(f32, f32)>) {
        let l = self.win().layout.expect("layout");
        let (sx, sy) = about.unwrap_or_else(|| {
            // About the focused item's centre, else the area centre.
            let w = self.win();
            w.canvas()
                .and_then(|c| c.focus.and_then(|id| c.item(id)))
                .map(|it| {
                    let sr = Self::item_screen_rect(w, l, it);
                    (sr.x + sr.w / 2.0, sr.y + sr.h / 2.0)
                })
                .unwrap_or((l.area.x + l.area.w / 2.0, l.area.y + l.area.h / 2.0))
        });
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.view.zoom_about(l.area, sx, sy, factor);
            c.focus_prev = None;
        }
        w.dirty = true;
        self.request_redraw();
    }

    pub(super) fn reset_zoom(&mut self) {
        let l = self.win().layout.expect("layout");
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            let center = c.focus.and_then(|id| c.item(id)).map(|it| it.rect.center());
            c.view.zoom = 1.0;
            if let Some((cx, cy)) = center {
                c.view.x = cx - l.area.w / 2.0;
                c.view.y = cy - l.area.h / 2.0;
            }
            c.focus_prev = None;
        }
        w.dirty = true;
        self.request_redraw();
    }

    /// Zoom out (or in) so that every item is visible.
    pub(super) fn fit_all(&mut self) {
        let l = self.win().layout.expect("layout");
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut()
            && let Some(b) = c.bounds() {
                c.view.fit(l.area, b, 40.0);
                if c.view.zoom > 1.0 {
                    // Fit never magnifies: cap at 1:1 and keep things centred.
                    c.view.zoom = 1.0;
                    let (cx, cy) = b.center();
                    c.view.x = cx - l.area.w / 2.0;
                    c.view.y = cy - l.area.h / 2.0;
                }
                c.focus_prev = None;
            }
        w.dirty = true;
        self.request_redraw();
    }

    /// Focus Mode: fill the view with the focused item; again to restore.
    pub(super) fn toggle_focus_mode(&mut self) {
        let l = self.win().layout.expect("layout");
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            if let Some(prev) = c.focus_prev.take() {
                c.view = prev;
            } else {
                // A selected group, a multi-selection, or the focused item.
                let target = c
                    .group_sel
                    .and_then(|g| c.group(g))
                    .map(|g| WRect::new(g.rect.x, g.rect.y - crate::canvas::GROUP_LABEL_H, g.rect.w, g.rect.h + crate::canvas::GROUP_LABEL_H))
                    .or_else(|| if c.selected.len() > 1 { c.bounds_of(&c.selected) } else { None })
                    .or_else(|| c.focus.and_then(|f| c.item(f)).map(|i| i.rect));
                if let Some(r) = target {
                    c.focus_prev = Some(c.view);
                    c.view.fit(l.area, r, 24.0);
                }
            }
        }
        w.dirty = true;
        self.request_redraw();
    }

    // -----------------------------------------------------------------------
    // Pointer interaction (called from input.rs)
    // -----------------------------------------------------------------------

    /// Handle a button event on the top bar, sidebar or canvas. Returns true
    /// if consumed.
    pub(super) fn canvas_mouse_button(&mut self, state: ElementState, button: MouseButton, event_loop: &ActiveEventLoop) -> bool {
        let (mx, my) = (self.win().mouse.x as f32, self.win().mouse.y as f32);
        let Some(l) = self.win().layout else { return false };

        // Finish drags on release.
        if state == ElementState::Released {
            let drag = self.win().cdrag.clone();
            match drag {
                CDrag::None => return false,
                CDrag::Move { item, moved, starts, .. } => {
                    if moved {
                        if self.is_pinned(item) {
                            self.settle_pin(item);
                        } else {
                            self.snap_item(item, false);
                            self.follow_primary(item, &starts);
                            self.settle_groups();
                        }
                        self.mark_dirty();
                    }
                }
                CDrag::Resize { item, .. } => {
                    if self.is_pinned(item) {
                        self.settle_pin(item);
                    } else {
                        self.snap_item(item, true);
                    }
                    self.apply_item_grid(item);
                    self.sync_mirror_sizes(item);
                    self.settle_groups();
                    self.mark_dirty();
                }
                CDrag::Pan { .. } => self.mark_dirty(),
                CDrag::Select { start, cur } => {
                    let band = WRect::new(start.0.min(cur.0), start.1.min(cur.1), (cur.0 - start.0).abs(), (cur.1 - start.1).abs());
                    if band.w > 2.0 || band.h > 2.0 {
                        self.select_in_rect(band);
                    } else {
                        self.clear_selection();
                    }
                }
                CDrag::MoveGroup { moved, .. } => {
                    if moved {
                        self.settle_groups();
                        self.mark_dirty();
                    }
                }
                CDrag::ResizeGroup { .. } => {
                    self.settle_groups();
                    self.mark_dirty();
                }
            }
            self.win_mut().cdrag = CDrag::None;
            self.request_redraw();
            return true;
        }

        if !l.area.contains(mx, my) {
            return false;
        }

        // Canvas area.
        match button {
            MouseButton::Middle => {
                self.win_mut().cdrag = CDrag::Pan { last: (mx, my) };
                return true;
            }
            MouseButton::Right => {
                if let Some((id, _)) = self.item_at(mx, my) {
                    self.focus_item(id);
                    self.open_item_menu(id, mx, my);
                } else if let Some((gid, GroupPart::Label)) = self.group_at(mx, my) {
                    if let Some(c) = self.win_mut().canvas_mut() {
                        c.group_sel = Some(gid);
                    }
                    self.open_group_menu(gid, mx, my);
                } else {
                    self.open_canvas_menu(mx, my);
                }
                return true;
            }
            MouseButton::Left => {}
            _ => return false,
        }

        if self.win().space_held || self.win().pan_mode {
            self.win_mut().cdrag = CDrag::Pan { last: (mx, my) };
            return true;
        }
        match self.item_at(mx, my) {
            Some((id, ItemPart::Close)) => {
                self.close_item(id, event_loop);
                true
            }
            Some((id, ItemPart::Title)) => {
                if self.mods.shift_key() {
                    self.toggle_selected(id);
                    return true;
                }
                let now = Instant::now();
                let dbl = matches!(self.win().last_title_click, Some((t, j)) if j == id && now.duration_since(t).as_millis() < 400);
                self.win_mut().last_title_click = Some((now, id));
                // Clicking outside the selection collapses it.
                if !self.win().canvas().map(|c| c.is_selected(id)).unwrap_or(false) {
                    self.clear_selection();
                }
                self.focus_item(id);
                if dbl {
                    self.win_mut().last_title_click = None;
                    self.start_item_rename(id);
                    return true;
                }
                let starts = self.drag_set(id);
                let (wx, wy) = self.item_point(id, mx, my);
                if let Some(it) = self.win().canvas().and_then(|c| c.item(id)) {
                    let grab = (wx - it.rect.x, wy - it.rect.y);
                    self.win_mut().cdrag = CDrag::Move { item: id, grab, moved: false, starts };
                }
                true
            }
            Some((id, ItemPart::Edge(edge))) => {
                self.clear_selection();
                self.focus_item(id);
                let (wx, wy) = self.item_point(id, mx, my);
                if let Some(it) = self.win().canvas().and_then(|c| c.item(id)) {
                    let start = it.rect;
                    self.win_mut().cdrag = CDrag::Resize { item: id, edge, start, press: (wx, wy) };
                }
                true
            }
            Some((id, ItemPart::Content)) => {
                if self.mods.shift_key() && self.win().canvas().and_then(|c| c.focus) != Some(id) {
                    // Shift+click on another terminal extends the item
                    // selection rather than the text selection.
                    self.toggle_selected(id);
                    return true;
                }
                if self.win().canvas().and_then(|c| c.focus) != Some(id) {
                    if !self.win().canvas().map(|c| c.is_selected(id)).unwrap_or(false) {
                        self.clear_selection();
                    }
                    self.focus_item(id);
                }
                // Text selection inside the terminal: existing path.
                self.on_left_button(ElementState::Pressed);
                true
            }
            None => {
                // Group frames sit behind items.
                match self.group_at(mx, my) {
                    Some((gid, GroupPart::Label)) => {
                        let now = Instant::now();
                        let key = ITEM_RENAME_BASE as u64 + gid; // distinct from item ids
                        let dbl = matches!(self.win().last_title_click, Some((t, j)) if j == key && now.duration_since(t).as_millis() < 400);
                        self.win_mut().last_title_click = Some((now, key));
                        if dbl {
                            self.win_mut().last_title_click = None;
                            self.start_group_rename(gid);
                            return true;
                        }
                        let (wx, wy) = self.win().canvas().map(|c| c.view.screen_to_world(l.area, mx, my)).unwrap_or((0.0, 0.0));
                        self.begin_group_move(gid, wx, wy);
                        self.request_redraw();
                        return true;
                    }
                    Some((gid, GroupPart::Edge(edge))) => {
                        let (wx, wy) = self.win().canvas().map(|c| c.view.screen_to_world(l.area, mx, my)).unwrap_or((0.0, 0.0));
                        let start = self.win().canvas().and_then(|c| c.group(gid)).map(|g| g.rect).unwrap_or_default();
                        if let Some(c) = self.win_mut().canvas_mut() {
                            c.group_sel = Some(gid);
                            c.selected.clear();
                        }
                        self.win_mut().cdrag = CDrag::ResizeGroup { group: gid, edge, start, press: (wx, wy) };
                        return true;
                    }
                    _ => {}
                }
                // Empty canvas: Shift+drag selects, plain drag pans; either
                // way the text selection and any item selection clear.
                if let Some(tab) = self.active_tab() {
                    tab.term.lock().selection = None;
                }
                self.clear_selection();
                if self.mods.shift_key() {
                    let (wx, wy) = self.win().canvas().map(|c| c.view.screen_to_world(l.area, mx, my)).unwrap_or((0.0, 0.0));
                    self.win_mut().cdrag = CDrag::Select { start: (wx, wy), cur: (wx, wy) };
                } else {
                    self.win_mut().cdrag = CDrag::Pan { last: (mx, my) };
                }
                true
            }
        }
    }

    /// Right-click on empty free canvas.
    pub(super) fn open_canvas_menu(&mut self, x: f32, y: f32) {
        let w = self.win();
        let Some(l) = w.layout else { return };
        let Some(c) = w.canvas() else { return };
        let (wx, wy) = c.view.screen_to_world(l.area, x, y);
        let one = c.items.len() == 1;
        let has_saved = !self.store.commands.is_empty();
        self.win_mut().menu = Some(Menu::for_canvas(x, y, wx, wy, has_saved, one));
        self.request_redraw();
    }

    /// Right-click on a terminal item.
    pub(super) fn open_item_menu(&mut self, id: ItemId, x: f32, y: f32) {
        let Some(tab) = self.tab_of_item(id) else {
            // An image: a short menu of its own.
            if self.win().canvas().and_then(|c| c.item(id)).map(|i| matches!(i.kind, ItemKind::Image { .. })).unwrap_or(false) {
                let pinned = self.is_pinned(id);
                self.win_mut().menu = Some(Menu::for_image(x, y, id, pinned));
                self.request_redraw();
            }
            return;
        };
        let w = self.win();
        let Some(l) = w.layout else { return };
        let Some(c) = w.canvas() else { return };
        let (wx, wy) = c.view.screen_to_world(l.area, x, y);
        let has_selection = w.term(tab).map(|t| t.term.lock().selection.as_ref().map(|s| !s.is_empty()).unwrap_or(false)).unwrap_or(false);
        let others: Vec<(usize, String)> = (0..w.canvases.len()).filter(|&i| i != w.active).map(|i| (i, w.tab_title(i))).collect();
        let has_saved = !self.store.commands.is_empty();
        let (pinned, mirror, monitor) = c.item(id).map(|i| (i.pin.is_some(), i.mirror, i.monitor)).unwrap_or((false, false, None));
        self.win_mut().menu = Some(Menu::for_item(x, y, wx, wy, tab, id, has_selection, has_saved, &others, pinned, mirror, monitor));
        self.request_redraw();
    }

    fn tab_of_item(&self, id: ItemId) -> Option<TabId> {
        match self.win().canvas()?.item(id)?.kind {
            ItemKind::Terminal(t) => Some(t),
            _ => None,
        }
    }

    /// Pointer moved: drive drags, else update hover/cursor. Returns true
    /// if the canvas consumed the motion (a drag is in progress).
    pub(super) fn canvas_mouse_move(&mut self) -> bool {
        let (mx, my) = (self.win().mouse.x as f32, self.win().mouse.y as f32);
        let Some(l) = self.win().layout else { return false };
        match self.win().cdrag.clone() {
            CDrag::Select { start, .. } => {
                let Some((wx, wy)) = self.win().canvas().map(|c| c.view.screen_to_world(l.area, mx, my)) else { return false };
                self.win_mut().cdrag = CDrag::Select { start, cur: (wx, wy) };
                self.set_cursor(CursorIcon::Crosshair);
                self.request_redraw();
                true
            }
            CDrag::MoveGroup { .. } => {
                let Some((wx, wy)) = self.win().canvas().map(|c| c.view.screen_to_world(l.area, mx, my)) else { return false };
                self.drag_group_move(wx, wy);
                true
            }
            CDrag::ResizeGroup { .. } => {
                let Some((wx, wy)) = self.win().canvas().map(|c| c.view.screen_to_world(l.area, mx, my)) else { return false };
                self.drag_group_resize(wx, wy);
                true
            }
            CDrag::Pan { last } => {
                let w = self.win_mut();
                if let Some(c) = w.canvas_mut() {
                    c.view.x -= (mx - last.0) / c.view.zoom;
                    c.view.y -= (my - last.1) / c.view.zoom;
                    c.focus_prev = None;
                }
                w.cdrag = CDrag::Pan { last: (mx, my) };
                self.set_cursor(CursorIcon::Grabbing);
                self.request_redraw();
                true
            }
            CDrag::Move { item, grab, moved, starts } => {
                let pinned = self.is_pinned(item);
                let (wx, wy) = self.item_point(item, mx, my);
                let w = self.win_mut();
                let mut now_moved = moved;
                if let Some(it) = w.canvas_mut().and_then(|c| c.item_mut(item)) {
                    let nx = (wx - grab.0).round();
                    let ny = (wy - grab.1).round();
                    let changed = (nx - it.rect.x).abs() > 0.5 || (ny - it.rect.y).abs() > 0.5;
                    it.rect.x = nx;
                    it.rect.y = ny;
                    now_moved = changed || moved;
                }
                // Live snap while moving; the rest of the selection follows.
                if !pinned {
                    self.snap_item(item, false);
                    self.follow_primary(item, &starts);
                }
                self.win_mut().cdrag = CDrag::Move { item, grab, moved: now_moved, starts };
                self.set_cursor(CursorIcon::Grabbing);
                self.request_redraw();
                true
            }
            CDrag::Resize { item, edge, start, press } => {
                let (wx, wy) = self.item_point(item, mx, my);
                let dx = wx - press.0;
                let dy = wy - press.1;
                let m = self.win().fonts.metrics;
                let (min_w, min_h) = self.win().rect_for_grid(MIN_COLS, MIN_ROWS);
                let mut r = start;
                if edge.right {
                    r.w = (start.w + dx).max(min_w);
                }
                if edge.bottom {
                    r.h = (start.h + dy).max(min_h);
                }
                if edge.left {
                    let new_w = (start.w - dx).max(min_w);
                    r.x = start.right() - new_w;
                    r.w = new_w;
                }
                if edge.top {
                    let new_h = (start.h - dy).max(min_h);
                    r.y = start.bottom() - new_h;
                    r.h = new_h;
                }
                let is_image = self.win().canvas().and_then(|c| c.item(item)).map(|i| matches!(i.kind, ItemKind::Image { .. })).unwrap_or(false);
                let (qw, qh) = if is_image {
                    // Keep the picture's aspect: width leads unless only a
                    // vertical edge is being dragged.
                    let aspect = ((start.w) / (start.h - TITLE_H).max(1.0)).max(0.05);
                    if (edge.top || edge.bottom) && !(edge.left || edge.right) {
                        let h = (r.h - TITLE_H).max(32.0);
                        ((h * aspect).round(), (h + TITLE_H).round())
                    } else {
                        let w = r.w.max(48.0);
                        (w.round(), (w / aspect + TITLE_H).round())
                    }
                } else {
                    // Quantise to whole cells so the grid always fills the frame.
                    let cols = ((r.w - 2.0 * ITEM_PAD) / m.width).floor().max(MIN_COLS as f32);
                    let rows = ((r.h - TITLE_H - 2.0 * ITEM_PAD) / m.height).floor().max(MIN_ROWS as f32);
                    ((cols * m.width + 2.0 * ITEM_PAD).round(), (rows * m.height + TITLE_H + 2.0 * ITEM_PAD).round())
                };
                if edge.left {
                    r.x = start.right() - qw;
                }
                if edge.top {
                    r.y = start.bottom() - qh;
                }
                r.w = qw;
                r.h = qh;
                let w = self.win_mut();
                if let Some(it) = w.canvas_mut().and_then(|c| c.item_mut(item)) {
                    it.rect = r;
                }
                self.apply_item_grid(item);
                self.sync_mirror_sizes(item);
                self.set_cursor(resize_cursor(edge));
                self.request_redraw();
                true
            }
            CDrag::None => {
                // Hover feedback.
                let over = if l.area.contains(mx, my) { self.item_at(mx, my) } else { None };
                let prev = self.win().hover_part;
                if over != prev {
                    self.win_mut().hover_part = over;
                    self.request_redraw();
                }
                if l.area.contains(mx, my) {
                    let icon = match over {
                        Some((_, ItemPart::Title)) => CursorIcon::Grab,
                        Some((_, ItemPart::Close)) => CursorIcon::Pointer,
                        Some((_, ItemPart::Edge(e))) => resize_cursor(e),
                        Some((_, ItemPart::Content)) => CursorIcon::Text,
                        None => match self.group_at(mx, my) {
                            Some((_, GroupPart::Label)) => CursorIcon::Grab,
                            Some((_, GroupPart::Edge(e))) => resize_cursor(e),
                            _ => {
                                if self.win().space_held || self.win().pan_mode { CursorIcon::Grab } else { CursorIcon::Default }
                            }
                        },
                    };
                    self.set_cursor(icon);
                }
                false
            }
        }
    }

    /// Wheel over the canvas: Ctrl zooms about the pointer; over a
    /// terminal it scrolls that terminal; over empty canvas it pans.
    pub(super) fn canvas_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (mx, my) = (self.win().mouse.x as f32, self.win().mouse.y as f32);
        let Some(l) = self.win().layout else { return false };
        if !l.area.contains(mx, my) {
            return false;
        }
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * 48.0, y * 48.0),
            MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
        };
        if self.mods.control_key() {
            let factor = (1.0 + dy / 400.0).clamp(0.5, 2.0);
            self.zoom_by(factor, Some((mx, my)));
            return true;
        }
        if let Some((id, ItemPart::Content)) | Some((id, ItemPart::Title)) = self.item_at(mx, my)
            && let Some(tab) = self.tab_of_item(id) {
                let lines = (dy / self.win().fonts.metrics.height * 1.0).round() as i32;
                let lines = if lines == 0 && dy != 0.0 { dy.signum() as i32 } else { lines };
                if lines != 0 {
                    self.scroll_tab(tab, lines);
                }
                return true;
            }
        // Pan (Shift swaps axes so a plain wheel can pan sideways).
        let (px, py) = if self.mods.shift_key() { (dy, dx) } else { (dx, dy) };
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.view.x -= px / c.view.zoom;
            c.view.y -= py / c.view.zoom;
            c.focus_prev = None;
        }
        w.dirty = true;
        self.request_redraw();
        true
    }

    /// Scroll a specific terminal's history (used by wheel-where-you-look).
    fn scroll_tab(&mut self, tab: TabId, lines: i32) {
        let Some(t) = self.win().term(tab) else { return };
        let mode = *t.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let key: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let key = if mode.contains(TermMode::APP_CURSOR) { key.to_vec() } else { key.replace_o() };
            let mut bytes = Vec::new();
            for _ in 0..lines.abs() {
                bytes.extend_from_slice(&key);
            }
            t.write(bytes);
        } else if !mode.contains(TermMode::ALT_SCREEN) {
            t.scroll(Scroll::Delta(lines));
        }
        self.request_redraw();
    }

    fn snap_item(&mut self, id: ItemId, resize: bool) {
        let Some(zoom) = self.win().canvas().map(|c| c.view.zoom) else { return };
        let tol = SNAP_PX / zoom;
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        if let Some(r) = c.item(id).map(|i| i.rect) {
            let snapped = c.snap(id, r, tol, resize);
            if let Some(it) = c.item_mut(id) {
                it.rect = snapped;
            }
        }
    }

    /// Resize an item's terminal to match its rect.
    fn apply_item_grid(&mut self, id: ItemId) {
        let w = self.win_mut();
        let Some((tab, rect)) = w.canvas().and_then(|c| c.item(id)).and_then(|i| match i.kind {
            ItemKind::Terminal(t) => Some((t, i.rect)),
            _ => None,
        }) else { return };
        let g = w.grid_for_rect(rect);
        if let Some(t) = w.term_mut(tab) {
            t.resize(g);
        }
    }

    /// Ctrl handling for double-Ctrl pan: call on every modifier change.
    pub(super) fn track_ctrl_tap(&mut self, was_ctrl: bool, now_ctrl: bool) {
        if was_ctrl && !now_ctrl {
            self.win_mut().last_ctrl_release = Some(Instant::now());
            if self.win().pan_mode {
                self.win_mut().pan_mode = false;
                if matches!(self.win().cdrag, CDrag::Pan { .. }) {
                    self.win_mut().cdrag = CDrag::None;
                }
                self.request_redraw();
            }
        } else if !was_ctrl && now_ctrl {
            let tapped = self.win().last_ctrl_release.map(|t| t.elapsed().as_millis() < 350).unwrap_or(false);
            if tapped {
                let (mx, my) = (self.win().mouse.x as f32, self.win().mouse.y as f32);
                let w = self.win_mut();
                w.pan_mode = true;
                w.cdrag = CDrag::Pan { last: (mx, my) };
            }
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    pub(super) fn draw_canvas(&mut self, l: Layout) {
        let theme = &self.theme;
        let anim_mode = self.config.terminal.cursor_animation.clone();
        let effects_cfg = self.effects.clone();
        let opacity = self.config.colors.opacity.clamp(0.3, 1.0);
        let epoch = self.epoch;
        let w = &mut self.wins[self.cur];
        let win_focused = w.focused;
        let area = l.area;
        let ci = w.active;
        if ci >= w.canvases.len() {
            return;
        }
        let view = w.canvases[ci].view;
        let focus = w.canvases[ci].focus;
        let hover = w.hover_part;
        let rename = w.rename.clone();
        let zoom = view.zoom;

        w.batch.push_clip(area.x, area.y, area.w, area.h);

        // Dot grid, every 64 world units, once dots are far enough apart.
        let step_px = 64.0 * zoom;
        if step_px >= 22.0 {
            let dot = with_alpha(theme.highlight, 0.55);
            let x0 = area.x - ((view.x * zoom) % step_px + step_px) % step_px;
            let y0 = area.y - ((view.y * zoom) % step_px + step_px) % step_px;
            let mut y = y0;
            while y < area.bottom() {
                let mut x = x0;
                while x < area.right() {
                    w.batch.rect(x, y, 2.0, 2.0, dot);
                    x += step_px;
                }
                y += step_px;
            }
        }

        Self::draw_groups(w, l, theme);

        // Items, back to front; pinned ones last so they sit on top.
        let mut ids: Vec<ItemId> = w.canvases[ci].items.iter().filter(|i| i.pin.is_none()).map(|i| i.id).collect();
        ids.extend(w.canvases[ci].items.iter().filter(|i| i.pin.is_some()).map(|i| i.id));
        for id in ids {
            let (rect, kind, pinned, mirror) = {
                let it = w.canvases[ci].item(id).expect("item");
                (it.rect, it.kind.clone(), it.pin.is_some(), it.mirror)
            };
            let zoom = if pinned { 1.0 } else { view.zoom };
            let sr = if pinned { Self::pinned_screen_rect(l, w.canvases[ci].item(id).expect("item")) } else { view.rect_to_screen(area, rect) };
            if !sr.intersects(&area) {
                continue;
            }
            if pinned {
                // Soft shadow so it reads as floating above the canvas.
                w.batch.rrect(sr.x + 2.0, sr.y + 3.0, sr.w, sr.h, 8.0, with_alpha(theme.bg, 0.55));
            }
            let focused = focus == Some(id);
            let selected = w.canvases[ci].is_selected(id);
            let hovered = hover.map(|(h, _)| h == id).unwrap_or(false);
            let title_h = TITLE_H * zoom;
            let radius = (8.0 * zoom).clamp(2.0, 10.0);
            // Frame + title bar.
            w.batch.rrect(sr.x, sr.y, sr.w, sr.h, radius, with_alpha(theme.bg, opacity.max(0.85)));
            w.batch.rrect(sr.x, sr.y, sr.w, title_h, radius, theme.tab_bar);
            w.batch.rect(sr.x, sr.y + title_h - radius.min(title_h), sr.w, radius.min(title_h), theme.tab_bar);
            if focused {
                w.batch.rect(sr.x, sr.y, sr.w, (2.0 * zoom).max(1.5), theme.accent);
            }
            let quiet = match kind {
                ItemKind::Terminal(t) => w.terms.iter().any(|x| x.id == t && x.quiet_alert),
                _ => false,
            };
            if let ItemKind::Image { path } = &kind {
                Self::ensure_image_loaded(w, id, path);
            }
            let border = if focused || selected { theme.accent } else if hovered { theme.muted } else { theme.highlight };
            w.batch.outline(sr.x, sr.y, sr.w, sr.h, radius, if focused { 1.5 } else { 1.0 }, border);
            if quiet {
                // Blink: on for 500ms, off for 300ms.
                let phase = (Instant::now().duration_since(epoch).as_millis() % 800) < 500;
                if phase {
                    let c = theme.ansi[3];
                    w.batch.outline(sr.x - 1.0, sr.y - 1.0, sr.w + 2.0, sr.h + 2.0, radius + 1.0, 2.5, c);
                    w.batch.rrect(sr.x, sr.y, sr.w, sr.h, radius, with_alpha(c, 0.08));
                }
            }
            if selected {
                w.batch.rrect(sr.x, sr.y, sr.w, sr.h, radius, with_alpha(theme.accent, 0.06));
            }

            // Title text and close button (skipped when too small to read).
            let tab_ref = match kind {
                ItemKind::Terminal(t) => w.terms.iter().position(|x| x.id == t),
                _ => None,
            };
            let image_name = w.canvases[ci].item(id).and_then(|i| i.name.clone());
            if title_h >= 12.0 {
                let fs = (12.0 * zoom).clamp(7.0, 18.0);
                let lh = w.fonts.line_height_for(fs * 1.12);
                let ty = sr.y + (title_h - lh) / 2.0;
                let pad = 10.0 * zoom;
                let close_w = title_h.max(12.0);
                // Item rename uses `rename` with the terminal's store index
                // offset by ITEM_RENAME_BASE so it cannot collide with a tab index.
                let renaming = matches!((&rename, tab_ref), (Some((ri, _, _)), Some(ti)) if *ri == ti + ITEM_RENAME_BASE);
                let mut title = match (&rename, tab_ref) {
                    (Some((_, text, _)), Some(_)) if renaming => text.clone(),
                    (_, Some(ti)) => w.terms[ti].display_title().to_string(),
                    _ if matches!(kind, ItemKind::Image { .. }) => image_name.clone().unwrap_or_else(|| "image".into()),
                    _ => "starting…".into(),
                };
                if !renaming {
                    if w.canvases[ci].item(id).map(|i| i.monitor.is_some()).unwrap_or(false) {
                        title = format!("◔ {title}");
                    }
                    if mirror {
                        title = format!("⧉ {title}");
                    }
                    if pinned {
                        title = format!("⌖ {title}");
                    }
                }
                let max_w = sr.w - pad * 2.0 - close_w;
                let color = if focused { theme.fg } else { scale_rgb(theme.fg, 0.75) };
                if renaming {
                    let all = rename.as_ref().map(|r| r.2).unwrap_or(false);
                    let tw = ui_text_width(&w.fonts, &title, fs);
                    if all && !title.is_empty() {
                        w.batch.rrect(sr.x + pad - 2.0, ty - 1.0, tw + 4.0, lh + 2.0, 3.0, theme.selection);
                    }
                    ui_text(&mut w.fonts, &mut w.batch, sr.x + pad, ty, &title, fs, theme.fg, false);
                    if !all {
                        w.batch.rect(sr.x + pad + tw + 1.0, ty, 2.0, lh, theme.cursor);
                    }
                } else {
                    let adv = w.fonts.advance_for(fs * 1.12);
                    let max_chars = (max_w / adv).floor().max(0.0) as usize;
                    let shown: String = if title.chars().count() > max_chars {
                        title.chars().take(max_chars.saturating_sub(1)).chain(std::iter::once('…')).collect()
                    } else {
                        title
                    };
                    ui_text(&mut w.fonts, &mut w.batch, sr.x + pad, ty, &shown, fs, color, focused);
                }
                // Close ×.
                let close_hover = matches!(hover, Some((h, ItemPart::Close)) if h == id);
                let cx = sr.right() - close_w;
                if close_hover {
                    w.batch.rrect(cx + 3.0, sr.y + 3.0, close_w - 6.0, title_h - 6.0, 4.0, with_alpha(theme.ansi[1], 0.9));
                }
                let xw = ui_text_width(&w.fonts, "×", fs);
                ui_text(&mut w.fonts, &mut w.batch, cx + (close_w - xw) / 2.0, ty, "×", fs, if close_hover { theme.fg } else { theme.muted }, false);
            }

            // Image content: letterboxed inside the frame, animated if it is.
            if matches!(kind, ItemKind::Image { .. }) {
                let cx = sr.x + 1.0;
                let cy = sr.y + title_h;
                let (cw, ch) = (sr.w - 2.0, sr.h - title_h - 1.0);
                w.batch.push_clip(cx, cy, cw, ch);
                let now_ms = Instant::now().duration_since(epoch).as_millis();
                let shown = w.images.get(&id).map(|img| (img.frame_at(now_ms), img.w, img.h, img.error.clone()));
                match shown {
                    Some((Some(tex), iw, ih, _)) if iw > 0 && ih > 0 => {
                        let f = (cw / iw as f32).min(ch / ih as f32);
                        let (dw, dh) = (iw as f32 * f, ih as f32 * f);
                        w.batch.image(tex, cx + (cw - dw) / 2.0, cy + (ch - dh) / 2.0, dw, dh, 1.0);
                    }
                    Some((_, _, _, Some(err))) => {
                        let msg = format!("couldn't load image: {err}");
                        ui_text(&mut w.fonts, &mut w.batch, cx + 8.0, cy + 8.0, &msg, 11.0, theme.muted, false);
                    }
                    _ => {}
                }
                w.batch.pop_clip();
            }

            // Content.
            if let Some(ti) = tab_ref {
                let cx = sr.x + ITEM_PAD * zoom;
                let cy = sr.y + (TITLE_H + ITEM_PAD) * zoom;
                let clip = (sr.x + 1.0, sr.y + title_h, sr.w - 2.0, sr.h - title_h - 1.0);
                let place = TermPlace { x: cx, y: cy, zoom, focused: focused && win_focused, clip: Some(clip) };
                let tab = &mut w.terms[ti];
                draw_term_view(&mut w.fonts, &mut w.batch, theme, &anim_mode, &effects_cfg, tab, place);
            }
        }

        Self::draw_group_labels(w, l, theme);
        Self::draw_selection_band(w, l, theme);

        // Empty-canvas hint.
        if w.canvases[ci].items.is_empty() {
            let msg = "Ctrl+Shift+Enter adds a terminal · right-click for more · Ctrl+wheel zooms";
            let fw = ui_text_width(&w.fonts, msg, 12.0);
            ui_text(&mut w.fonts, &mut w.batch, area.x + (area.w - fw) / 2.0, area.y + area.h / 2.0, msg, 12.0, theme.muted, false);
        }
        w.batch.pop_clip();
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Snapshot every window's canvases into `state.json`.
    pub(super) fn save_state(&mut self) {
        let mut st = SavedState { version: 1, windows: Vec::new() };
        for w in &self.wins {
            let mut canvases = w.canvases.clone();
            for c in &mut canvases {
                for it in &mut c.items {
                    if let ItemKind::Terminal(t) = it.kind {
                        // The terminal itself is not persisted (yet): keep its
                        // launch spec and custom title so it can be recreated.
                        if let Some(x) = w.term(t) {
                            let spec = it.launch.get_or_insert_with(|| LaunchSpec { shortcut: x.shortcut.clone(), cwd: None, session: None });
                            spec.session = x.session.clone();
                            if x.custom_title.is_some() {
                                it.name = x.custom_title.clone();
                            }
                        }
                        it.kind = ItemKind::Pending;
                    }
                }
            }
            let size = w.window.inner_size();
            st.windows.push(SavedWindow { width: size.width, height: size.height, canvases, active: w.active });
        }
        if let Err(e) = st.save() {
            log::warn!("could not save state: {e}");
        }
        for w in &mut self.wins {
            w.dirty = false;
        }
        self.last_save = Instant::now();
    }

    /// Attach every live session host nothing claimed and put them on a
    /// "Recovered" canvas tab in the current window.
    pub(super) fn adopt_orphans(&mut self) {
        if self.wins.is_empty() {
            return;
        }
        let claimed: std::collections::HashSet<String> =
            self.wins.iter().flat_map(|w| w.terms.iter().filter_map(|t| t.session.clone())).collect();
        let orphans: Vec<String> = crate::session::list_ids().into_iter().filter(|id| !claimed.contains(id)).collect();
        if orphans.is_empty() {
            return;
        }
        let Some(l) = self.win().layout else { return };
        let (iw, ih) = self.win().rect_for_grid(80, 24);
        let mut canvas: Option<Canvas> = None;
        for sid in orphans {
            let grid = self.win().grid_for_rect(WRect::new(0.0, 0.0, iw, ih));
            let id = self.next_id;
            self.next_id += 1;
            let term = match Terminal::attach(id, self.proxy.clone(), &sid, grid, self.term_config(), "shell".into()) {
                Ok(t) => t,
                Err(e) => {
                    log::info!("dropping stale session {sid}: {e:#}");
                    crate::session::remove_files(&sid);
                    continue;
                }
            };
            let c = canvas.get_or_insert_with(|| {
                let cid = self.next_canvas_id;
                self.next_canvas_id += 1;
                Canvas::new(cid, "Recovered")
            });
            let rect = c.spawn_rect(l.area, iw, ih);
            let item_id = self.next_item_id;
            self.next_item_id += 1;
            c.items.push(Item { id: item_id, kind: ItemKind::Terminal(id), rect, name: None, launch: Some(LaunchSpec { shortcut: None, cwd: None, session: Some(sid.clone()) }), pin: None, mirror: false, monitor: None });
            c.focus = Some(item_id);
            self.win_mut().terms.push(term);
            log::info!("recovered session {sid}");
        }
        if let Some(mut c) = canvas {
            let n = c.items.len();
            if let Some(b) = c.bounds() {
                c.view.fit(l.area, b, 40.0);
                if c.view.zoom > 1.0 {
                    c.view.zoom = 1.0;
                    let (cx, cy) = b.center();
                    c.view.x = cx - l.area.w / 2.0;
                    c.view.y = cy - l.area.h / 2.0;
                }
            }
            let w = self.win_mut();
            w.canvases.push(c);
            w.active = w.canvases.len() - 1;
            w.dirty = true;
            self.set_status(format!("recovered {n} running shell{}", if n == 1 { "" } else { "s" }));
            self.update_window_title();
            self.request_redraw();
        }
    }

    /// Recreate a saved window's tabs and terminals into window `wi`.
    pub(super) fn restore_window(&mut self, wi: usize, saved: SavedWindow) {
        self.cur = wi;
        let _ = self.win().window.request_inner_size(PhysicalSize::new(saved.width.max(400), saved.height.max(300)));
        let mut canvases = saved.canvases;
        canvases.retain(|c| !(c.is_single() && c.items.is_empty()));
        if canvases.is_empty() {
            let launch = self.shell_launch();
            self.open_tab(launch);
            return;
        }
        for c in &canvases {
            self.next_canvas_id = self.next_canvas_id.max(c.id + 1);
            for it in &c.items {
                self.next_item_id = self.next_item_id.max(it.id + 1);
            }
        }
        let n = canvases.len();
        for c in &mut canvases {
            for g in &c.groups {
                self.next_group_id = self.next_group_id.max(g.id + 1);
            }
        }
        self.win_mut().canvases = canvases;
        let l = self.win().layout.expect("layout");
        for ci in 0..n {
            self.win_mut().active = ci;
            let single = self.win().canvases[ci].is_single();
            let mut pending: Vec<PendingItem> = self.win().canvases[ci]
                .items
                .iter()
                .filter(|i| matches!(i.kind, ItemKind::Pending))
                .map(|i| (i.id, i.rect, i.launch.clone(), i.name.clone(), i.mirror))
                .collect();
            // Owners first so mirrors find their terminal.
            pending.sort_by_key(|p| p.4);
            for (item_id, rect, spec, name, mirror) in pending {
                if mirror {
                    // A mirror shows a terminal restored just above (same
                    // session id); without one it has nothing to show.
                    let owner = spec.as_ref().and_then(|s| s.session.as_deref()).and_then(|sid| {
                        let w = self.win();
                        w.canvases[ci].items.iter().find_map(|i| match i.kind {
                            ItemKind::Terminal(t) if w.term(t).and_then(|x| x.session.as_deref()) == Some(sid) => Some(t),
                            _ => None,
                        })
                    });
                    let w = self.win_mut();
                    match owner {
                        Some(t) => {
                            if let Some(it) = w.canvases[ci].item_mut(item_id) {
                                it.kind = ItemKind::Terminal(t);
                            }
                        }
                        None => w.canvases[ci].items.retain(|i| i.id != item_id),
                    }
                    continue;
                }
                let launch = match spec.as_ref().and_then(|s| s.shortcut.as_deref()) {
                    Some(n) => match self.store.commands.iter().find(|c| c.name == n).cloned() {
                        Some(cmd) => self.command_launch(&cmd),
                        None => self.shell_launch(),
                    },
                    None => self.shell_launch(),
                };
                let grid = if single { Self::grid_size_of(self.win()) } else { self.win().grid_for_rect(rect) };
                let id = self.next_id;
                self.next_id += 1;
                // A still-running detached session comes back as it was;
                // otherwise start the shell afresh.
                let attached = spec
                    .as_ref()
                    .and_then(|s| s.session.as_deref())
                    .filter(|sid| crate::session::socket_path(sid).exists())
                    .and_then(|sid| match Terminal::attach(id, self.proxy.clone(), sid, grid, self.term_config(), launch.title.clone()) {
                        Ok(mut t) => {
                            t.shortcut = spec.as_ref().and_then(|s| s.shortcut.clone());
                            Some(t)
                        }
                        Err(e) => {
                            log::warn!("session {sid} not reachable ({e:#}); starting a new shell");
                            crate::session::remove_files(sid);
                            None
                        }
                    });
                let spawned = match attached {
                    Some(t) => Ok(t),
                    None => self.launch_terminal(id, &launch, grid),
                };
                match spawned {
                    Ok(mut term) => {
                        term.custom_title = name;
                        let w = self.win_mut();
                        w.terms.push(term);
                        if let Some(it) = w.canvases[ci].item_mut(item_id) {
                            it.kind = ItemKind::Terminal(id);
                            it.launch = spec;
                            if single {
                                it.rect = WRect::new(0.0, 0.0, l.area.w, l.area.h);
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("restore: failed to spawn terminal: {e:#}");
                        self.win_mut().canvases[ci].items.retain(|i| i.id != item_id);
                    }
                }
            }
            let w = self.win_mut();
            let c = &mut w.canvases[ci];
            if c.focus.and_then(|f| c.item(f)).is_none() {
                c.focus = c.items.last().map(|i| i.id);
            }
        }
        self.win_mut().active = saved.active.min(n - 1);
        self.relayout();
        self.update_window_title();
    }

    /// Rename a terminal item on a free canvas (inline in its title bar).
    pub(super) fn start_item_rename(&mut self, id: ItemId) {
        let Some(tab) = self.tab_of_item(id) else { return };
        let Some(ti) = self.win().term_index(tab) else { return };
        let current = self.win().terms[ti].display_title().to_string();
        let w = self.win_mut();
        w.cdrag = CDrag::None;
        w.rename = Some((ti + ITEM_RENAME_BASE, current, true));
        self.request_redraw();
    }
}

/// A saved item awaiting its terminal: (id, rect, launch, name, mirror).
type PendingItem = (ItemId, WRect, Option<LaunchSpec>, Option<String>, bool);

/// Rename targets at or above this index refer to terminals in the store
/// (item renames); below it they are tab indices.
pub(super) const ITEM_RENAME_BASE: usize = 1 << 20;

pub(super) fn resize_cursor(e: Edge) -> CursorIcon {
    match (e.left, e.right, e.top, e.bottom) {
        (true, _, true, _) | (_, true, _, true) => CursorIcon::NwseResize,
        (true, _, _, true) | (_, true, true, _) => CursorIcon::NeswResize,
        (true, _, _, _) | (_, true, _, _) => CursorIcon::EwResize,
        _ => CursorIcon::NsResize,
    }
}

#[allow(dead_code)]
const _: (f32, f32, f32) = (EDGE_PX, MIN_ZOOM, MAX_ZOOM);
#[allow(dead_code)]
fn _viewport_type_check(_v: Viewport) {}
