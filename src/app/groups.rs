//! Multi-selection and groups on a free canvas: rubber-band and
//! Shift+click selection, moving several items at once, and named group
//! frames that move their members and pick up membership by containment.

use super::*;
use crate::canvas::{group_part, Group, GroupId, GroupPart, GROUP_LABEL_H, GROUP_MIN, GROUP_PAD};

/// Rename targets at or above this index are group ids (see
/// `ITEM_RENAME_BASE` for terminals).
pub(super) const GROUP_RENAME_BASE: usize = 1 << 21;

/// Tints cycle through these palette indexes as groups are created.
const GROUP_COLORS: [u8; 6] = [4, 2, 5, 3, 6, 1];

impl App {
    // -----------------------------------------------------------------------
    // Geometry
    // -----------------------------------------------------------------------

    /// Screen rect of a group's name tab (drawn above its frame).
    pub(super) fn group_label_rect(w: &Win, l: Layout, g: &Group) -> WRect {
        let Some(c) = w.canvas() else { return WRect::new(0.0, 0.0, 0.0, 0.0) };
        let z = c.view.zoom;
        let fs = (12.0 * z).clamp(7.0, 18.0);
        let tw = ui_text_width(&w.fonts, &g.name, fs) + 20.0 * z;
        let frame = c.view.rect_to_screen(l.area, g.rect);
        let h = (GROUP_LABEL_H * z).max(10.0);
        WRect::new(frame.x, frame.y - h, tw.max(60.0 * z).min(frame.w.max(60.0 * z)), h)
    }

    /// Group and part under a screen point (topmost first). Items are
    /// hit-tested before this, so `Inside` means empty space in a group.
    pub(super) fn group_at(&self, sx: f32, sy: f32) -> Option<(GroupId, GroupPart)> {
        let w = self.win();
        let l = w.layout?;
        let c = w.canvas()?;
        for g in c.groups.iter().rev() {
            let frame = c.view.rect_to_screen(l.area, g.rect);
            let label = Self::group_label_rect(w, l, g);
            if let Some(part) = group_part(frame, label, sx, sy) {
                return Some((g.id, part));
            }
        }
        None
    }

    // -----------------------------------------------------------------------
    // Selection
    // -----------------------------------------------------------------------

    pub(super) fn clear_selection(&mut self) {
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.selected.clear();
            c.group_sel = None;
        }
        self.request_redraw();
    }

    /// Shift+click: add or remove one item from the selection.
    pub(super) fn toggle_selected(&mut self, id: ItemId) {
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.group_sel = None;
            if let Some(f) = c.focus
                && c.selected.is_empty()
                && f != id
            {
                // First Shift+click: the focused item joins too, so the
                // selection is "what I had, plus this".
                c.selected.push(f);
            }
            if let Some(p) = c.selected.iter().position(|&i| i == id) {
                c.selected.remove(p);
            } else {
                c.selected.push(id);
            }
        }
        self.request_redraw();
    }

    /// Rubber band finished: select everything it touched.
    pub(super) fn select_in_rect(&mut self, band: WRect) {
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.group_sel = None;
            c.selected = c.items.iter().filter(|i| i.pin.is_none() && i.rect.intersects(&band)).map(|i| i.id).collect();
            if let Some(&last) = c.selected.last()
                && !c.focus.map(|f| c.selected.contains(&f)).unwrap_or(false)
            {
                c.focus = Some(last);
            }
        }
        let focus_tab = self.win().canvas().and_then(|c| c.focus).and_then(|f| self.tab_of_item_pub(f));
        if let Some(t) = focus_tab {
            self.focus_terminal(t);
        }
        self.request_redraw();
    }

    pub(super) fn tab_of_item_pub(&self, id: ItemId) -> Option<TabId> {
        match self.win().canvas()?.item(id)?.kind {
            ItemKind::Terminal(t) => Some(t),
            _ => None,
        }
    }

    // -----------------------------------------------------------------------
    // Groups
    // -----------------------------------------------------------------------

    /// Ctrl+Shift+G: group the selection (or the focused item). If the
    /// selection is exactly an existing group, dissolve it instead.
    pub(super) fn toggle_group(&mut self) {
        if !self.on_free_canvas() {
            self.set_status("Groups live on canvas tabs (Ctrl+Shift+Enter turns this one into a canvas)".into());
            return;
        }
        // A selected group (its label was clicked) and no item selection:
        // dissolve it.
        if let Some(gid) = self.win().canvas().filter(|c| c.selected.is_empty()).and_then(|c| c.group_sel) {
            self.ungroup(gid);
            self.set_status("group dissolved".into());
            return;
        }
        let ids = self.win().canvas().map(|c| c.selection_or_focus()).unwrap_or_default();
        if ids.is_empty() {
            self.set_status("Select terminals first: Shift+click or Shift+drag on the canvas".into());
            return;
        }
        let existing = self.win().canvas().and_then(|c| {
            c.groups.iter().find(|g| {
                g.members.len() == ids.len() && ids.iter().all(|i| g.members.contains(i))
            }).map(|g| g.id)
        });
        if let Some(gid) = existing {
            self.ungroup(gid);
            self.set_status("group dissolved".into());
            return;
        }
        let gid = self.next_group_id;
        self.next_group_id += 1;
        let n_before = self.win().canvas().map(|c| c.groups.len()).unwrap_or(0);
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        let Some(b) = c.bounds_of(&ids) else { return };
        let rect = WRect::new(b.x - GROUP_PAD, b.y - GROUP_PAD, b.w + 2.0 * GROUP_PAD, b.h + 2.0 * GROUP_PAD);
        // Leave any previous group.
        for g in &mut c.groups {
            g.members.retain(|m| !ids.contains(m));
        }
        c.groups.retain(|g| !g.members.is_empty());
        let color = GROUP_COLORS[n_before % GROUP_COLORS.len()];
        c.groups.push(Group { id: gid, name: format!("Group {}", n_before + 1), rect, color, members: ids.clone() });
        c.recompute_membership();
        c.group_sel = Some(gid);
        c.selected.clear();
        w.dirty = true;
        self.set_status("grouped · drag the label to move all · double-click it to rename".into());
        self.request_redraw();
    }

    pub(super) fn ungroup(&mut self, gid: GroupId) {
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.groups.retain(|g| g.id != gid);
            if c.group_sel == Some(gid) {
                c.group_sel = None;
            }
        }
        w.dirty = true;
        self.request_redraw();
    }

    /// Zoom the view to a group's frame (used by Focus Mode and the menu).
    pub(super) fn zoom_to_group(&mut self, gid: GroupId) {
        let Some(l) = self.win().layout else { return };
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut()
            && let Some(g) = c.group(gid)
        {
            let r = WRect::new(g.rect.x, g.rect.y - GROUP_LABEL_H, g.rect.w, g.rect.h + GROUP_LABEL_H);
            if c.focus_prev.is_none() {
                c.focus_prev = Some(c.target_view());
            }
            let mut v = c.target_view();
            v.fit(l.area, r, 24.0);
            c.glide_to(v, l.area);
            c.group_sel = Some(gid);
        }
        w.dirty = true;
        self.request_redraw();
    }

    pub(super) fn close_group(&mut self, gid: GroupId, event_loop: &ActiveEventLoop) {
        let tabs: Vec<TabId> = self
            .win()
            .canvas()
            .and_then(|c| c.group(gid).map(|g| g.members.iter().filter_map(|&m| c.item(m)).filter_map(|i| match i.kind { ItemKind::Terminal(t) => Some(t), _ => None }).collect()))
            .unwrap_or_default();
        for t in tabs {
            self.close_terminal(t, event_loop);
        }
        self.ungroup(gid);
    }

    pub(super) fn start_group_rename(&mut self, gid: GroupId) {
        let Some(name) = self.win().canvas().and_then(|c| c.group(gid)).map(|g| g.name.clone()) else { return };
        let w = self.win_mut();
        w.cdrag = CDrag::None;
        w.rename = Some((GROUP_RENAME_BASE + gid as usize, name, true));
        self.request_redraw();
    }

    pub(super) fn open_group_menu(&mut self, gid: GroupId, x: f32, y: f32) {
        let Some(name) = self.win().canvas().and_then(|c| c.group(gid)).map(|g| g.name.clone()) else { return };
        self.win_mut().menu = Some(Menu::for_group(x, y, gid, &name));
        self.request_redraw();
    }

    // -----------------------------------------------------------------------
    // Drags
    // -----------------------------------------------------------------------

    /// Start rects of every item that should follow a drag of `primary`:
    /// the multi-selection when it contains the primary, else the primary alone.
    pub(super) fn drag_set(&self, primary: ItemId) -> Vec<(ItemId, WRect)> {
        let Some(c) = self.win().canvas() else { return Vec::new() };
        let primary_pinned = c.item(primary).map(|i| i.pin.is_some()).unwrap_or(false);
        let mut ids: Vec<ItemId> = if c.is_selected(primary) && !primary_pinned {
            c.selected.iter().copied().filter(|&i| c.item(i).map(|x| x.pin.is_none()).unwrap_or(false)).collect()
        } else {
            vec![primary]
        };
        if !ids.contains(&primary) {
            ids.push(primary);
        }
        ids.iter().filter_map(|&id| c.item(id).map(|i| (id, i.rect))).collect()
    }

    /// Apply the primary item's (already snapped) displacement to the
    /// other items of the drag set.
    pub(super) fn follow_primary(&mut self, primary: ItemId, starts: &[(ItemId, WRect)]) {
        let Some(c) = self.win_mut().canvas_mut() else { return };
        let Some(p0) = starts.iter().find(|(id, _)| *id == primary).map(|(_, r)| *r) else { return };
        let Some(p1) = c.item(primary).map(|i| i.rect) else { return };
        let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
        for (id, r0) in starts {
            if *id == primary {
                continue;
            }
            if let Some(it) = c.item_mut(*id) {
                it.rect.x = r0.x + dx;
                it.rect.y = r0.y + dy;
            }
        }
    }

    pub(super) fn begin_group_move(&mut self, gid: GroupId, wx: f32, wy: f32) {
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        let Some(g) = c.group(gid) else { return };
        let grab = (wx - g.rect.x, wy - g.rect.y);
        let starts: Vec<(ItemId, WRect)> = g.members.iter().filter_map(|&m| c.item(m).map(|i| (m, i.rect))).collect();
        let rect0 = g.rect;
        c.group_sel = Some(gid);
        c.selected.clear();
        // Raise members so the group reads as one thing.
        for (m, _) in &starts {
            c.raise(*m);
        }
        w.cdrag = CDrag::MoveGroup { group: gid, grab, rect0, starts, moved: false };
    }

    pub(super) fn drag_group_move(&mut self, wx: f32, wy: f32) {
        let CDrag::MoveGroup { group, grab, rect0, starts, .. } = self.win().cdrag.clone() else { return };
        let nx = (wx - grab.0).round();
        let ny = (wy - grab.1).round();
        let (dx, dy) = (nx - rect0.x, ny - rect0.y);
        let w = self.win_mut();
        let Some(c) = w.canvas_mut() else { return };
        if let Some(g) = c.group_mut(group) {
            g.rect.x = nx;
            g.rect.y = ny;
        }
        for (id, r0) in &starts {
            if let Some(it) = c.item_mut(*id) {
                it.rect.x = r0.x + dx;
                it.rect.y = r0.y + dy;
            }
        }
        if let CDrag::MoveGroup { moved, .. } = &mut w.cdrag {
            *moved = *moved || dx.abs() > 0.5 || dy.abs() > 0.5;
        }
        self.set_cursor(CursorIcon::Grabbing);
        self.request_redraw();
    }

    pub(super) fn drag_group_resize(&mut self, wx: f32, wy: f32) {
        let CDrag::ResizeGroup { group, edge, start, press } = self.win().cdrag.clone() else { return };
        let dx = wx - press.0;
        let dy = wy - press.1;
        let mut r = start;
        if edge.right {
            r.w = (start.w + dx).max(GROUP_MIN);
        }
        if edge.bottom {
            r.h = (start.h + dy).max(GROUP_MIN);
        }
        if edge.left {
            let nw = (start.w - dx).max(GROUP_MIN);
            r.x = start.right() - nw;
            r.w = nw;
        }
        if edge.top {
            let nh = (start.h - dy).max(GROUP_MIN);
            r.y = start.bottom() - nh;
            r.h = nh;
        }
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            if let Some(g) = c.group_mut(group) {
                g.rect = WRect::new(r.x.round(), r.y.round(), r.w.round(), r.h.round());
            }
            c.recompute_membership();
        }
        self.set_cursor(super::canvas_ui::resize_cursor(edge));
        self.request_redraw();
    }

    /// Group membership follows geometry: call after any move or resize.
    pub(super) fn settle_groups(&mut self) {
        let w = self.win_mut();
        if let Some(c) = w.canvas_mut() {
            c.recompute_membership();
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    /// Group frames and labels; drawn before items.
    pub(super) fn draw_groups(w: &mut Win, l: Layout, theme: &Theme) {
        let ci = w.active;
        let Some(c) = w.canvases.get(ci) else { return };
        let view = c.view;
        let z = view.zoom;
        let sel = c.group_sel;
        let groups: Vec<Group> = c.groups.clone();
        for g in &groups {
            let fr = view.rect_to_screen(l.area, g.rect);
            if !fr.intersects(&l.area) {
                continue;
            }
            let tint = theme.ansi[(g.color as usize).min(15)];
            let active = sel == Some(g.id);
            let radius = (10.0 * z).clamp(3.0, 12.0);
            w.batch.rrect(fr.x, fr.y, fr.w, fr.h, radius, with_alpha(tint, if active { 0.12 } else { 0.07 }));
            // Dashed outline.
            let dash = (10.0 * z).max(4.0);
            let gap = (6.0 * z).max(3.0);
            let th = if active { 1.5 } else { 1.0 };
            let col = with_alpha(tint, if active { 0.95 } else { 0.6 });
            let mut x = fr.x + radius;
            while x < fr.right() - radius {
                let wdt = dash.min(fr.right() - radius - x);
                w.batch.rect(x, fr.y, wdt, th, col);
                w.batch.rect(x, fr.bottom() - th, wdt, th, col);
                x += dash + gap;
            }
            let mut y = fr.y + radius;
            while y < fr.bottom() - radius {
                let h = dash.min(fr.bottom() - radius - y);
                w.batch.rect(fr.x, y, th, h, col);
                w.batch.rect(fr.right() - th, y, th, h, col);
                y += dash + gap;
            }
        }
    }

    /// Group name tabs; drawn after items so they stay readable.
    pub(super) fn draw_group_labels(w: &mut Win, l: Layout, theme: &Theme) {
        let ci = w.active;
        let Some(c) = w.canvases.get(ci) else { return };
        let z = c.view.zoom;
        let sel = c.group_sel;
        let groups: Vec<Group> = c.groups.clone();
        for g in &groups {
            let tint = theme.ansi[(g.color as usize).min(15)];
            let active = sel == Some(g.id);
            let radius = (10.0 * z).clamp(3.0, 12.0);
            let label = Self::group_label_rect(w, l, g);
            if label.intersects(&l.area) && label.h >= 10.0 {
                w.batch.rrect(label.x, label.y, label.w, label.h + radius.min(label.h), radius.min(label.h), with_alpha(tint, if active { 0.9 } else { 0.7 }));
                w.batch.rect(label.x, label.y + label.h - 1.0, label.w, 1.0, with_alpha(tint, 0.9));
                let fs = (12.0 * z).clamp(7.0, 18.0);
                let lh = w.fonts.line_height_for(fs * 1.12);
                let name = if w.rename.as_ref().map(|(i, _, _)| *i == GROUP_RENAME_BASE + g.id as usize).unwrap_or(false) {
                    w.rename.as_ref().map(|(_, t, _)| t.clone()).unwrap_or_default()
                } else {
                    g.name.clone()
                };
                let renaming = w.rename.as_ref().map(|(i, _, _)| *i == GROUP_RENAME_BASE + g.id as usize).unwrap_or(false);
                let tx = label.x + 10.0 * z;
                let ty = label.y + (label.h - lh) / 2.0;
                if renaming {
                    let all = w.rename.as_ref().map(|r| r.2).unwrap_or(false);
                    let tw = ui_text_width(&w.fonts, &name, fs);
                    if all && !name.is_empty() {
                        w.batch.rrect(tx - 2.0, ty - 1.0, tw + 4.0, lh + 2.0, 3.0, theme.selection);
                    }
                    ui_text(&mut w.fonts, &mut w.batch, tx, ty, &name, fs, theme.bg, true);
                    if !all {
                        w.batch.rect(tx + tw + 1.0, ty, 2.0, lh, theme.bg);
                    }
                } else {
                    ui_text(&mut w.fonts, &mut w.batch, tx, ty, &name, fs, theme.bg, true);
                }
            }
        }
    }

    /// Rubber-band rectangle while selecting.
    pub(super) fn draw_selection_band(w: &mut Win, l: Layout, theme: &Theme) {
        let CDrag::Select { start, cur } = w.cdrag else { return };
        let Some(c) = w.canvas() else { return };
        let band = WRect::new(start.0.min(cur.0), start.1.min(cur.1), (cur.0 - start.0).abs(), (cur.1 - start.1).abs());
        let sr = c.view.rect_to_screen(l.area, band);
        w.batch.rrect(sr.x, sr.y, sr.w, sr.h, 2.0, with_alpha(theme.accent, 0.12));
        w.batch.outline(sr.x, sr.y, sr.w, sr.h, 2.0, 1.0, with_alpha(theme.accent, 0.8));
    }
}
