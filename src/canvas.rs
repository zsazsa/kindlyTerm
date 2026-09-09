//! The Canvas model: an infinite surface with a viewport, holding items
//! (terminals, later images) at world coordinates. Pure data and geometry;
//! drawing and input live in `app/canvas_ui.rs`.
//!
//! World units are pixels at zoom 1. The viewport maps world to screen:
//! `screen = area.origin + (world - view.origin) * view.zoom`.

use serde::{Deserialize, Serialize};

use crate::terminal::TabId;

pub type CanvasId = u64;
pub type ItemId = u64;

/// Title bar height of an item, in world units.
pub const TITLE_H: f32 = 28.0;
/// Inner padding between an item's frame and its grid, in world units.
pub const ITEM_PAD: f32 = 6.0;
/// Snap distance in screen pixels.
pub const SNAP_PX: f32 = 8.0;
/// Resize handle thickness in screen pixels.
pub const EDGE_PX: f32 = 7.0;

pub const MIN_ZOOM: f32 = 0.15;
pub const MAX_ZOOM: f32 = 4.0;

#[derive(Default, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl WRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    /// Does this rect fully contain `o`?
    pub fn contains_rect(&self, o: &WRect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }

    pub fn intersects(&self, o: &WRect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn union(&self, o: &WRect) -> WRect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        WRect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// World coordinate shown at the canvas area's top-left corner.
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, zoom: 1.0 }
    }
}

impl Viewport {
    pub fn world_to_screen(&self, area: WRect, wx: f32, wy: f32) -> (f32, f32) {
        (area.x + (wx - self.x) * self.zoom, area.y + (wy - self.y) * self.zoom)
    }
    pub fn screen_to_world(&self, area: WRect, sx: f32, sy: f32) -> (f32, f32) {
        ((sx - area.x) / self.zoom + self.x, (sy - area.y) / self.zoom + self.y)
    }
    pub fn rect_to_screen(&self, area: WRect, r: WRect) -> WRect {
        let (x, y) = self.world_to_screen(area, r.x, r.y);
        WRect { x, y, w: r.w * self.zoom, h: r.h * self.zoom }
    }
    /// Zoom by `factor` keeping the world point under screen (sx, sy) fixed.
    pub fn zoom_about(&mut self, area: WRect, sx: f32, sy: f32, factor: f32) {
        let (wx, wy) = self.screen_to_world(area, sx, sy);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        // Solve for the new origin so (wx, wy) stays under (sx, sy).
        self.x = wx - (sx - area.x) / self.zoom;
        self.y = wy - (sy - area.y) / self.zoom;
    }
    /// Fit a world rect into the area with a margin (in screen px).
    pub fn fit(&mut self, area: WRect, target: WRect, margin: f32) {
        let avail_w = (area.w - 2.0 * margin).max(1.0);
        let avail_h = (area.h - 2.0 * margin).max(1.0);
        let zoom = (avail_w / target.w.max(1.0)).min(avail_h / target.h.max(1.0)).clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom = zoom;
        let (cx, cy) = target.center();
        self.x = cx - area.w / 2.0 / zoom;
        self.y = cy - area.h / 2.0 / zoom;
    }
    /// World rect currently visible.
    pub fn visible(&self, area: WRect) -> WRect {
        WRect { x: self.x, y: self.y, w: area.w / self.zoom, h: area.h / self.zoom }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ItemKind {
    /// A terminal; the id refers to a `Terminal` in the window's store.
    /// Not serialised directly: persistence stores the launch spec instead.
    #[serde(skip)]
    Terminal(TabId),
    /// Placeholder for a terminal that has not been (re)created yet.
    Pending,
}

/// Screen corner a pinned item keeps its distance from when the window
/// resizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub kind: ItemKind,
    /// Frame rectangle in world units (title bar included). For a pinned
    /// item: pixels relative to the canvas area's top-left, at zoom 1.
    pub rect: WRect,
    /// Pinned to the screen: drawn on top in screen space, unaffected by
    /// pan and zoom, anchored to this corner.
    #[serde(default)]
    pub pin: Option<Corner>,
    /// A second view of a terminal that another item on this canvas owns.
    /// Closing a mirror only removes the item.
    #[serde(default)]
    pub mirror: bool,
    /// Inactivity monitor: after this many seconds without output the
    /// frame blinks until output resumes (a finished build, a stalled job).
    #[serde(default)]
    pub monitor: Option<u32>,
    /// User-given name (overrides the shell title).
    #[serde(default)]
    pub name: Option<String>,
    /// How this item's shell was started, for persistence and relaunch.
    #[serde(default)]
    pub launch: Option<LaunchSpec>,
}

/// Enough to recreate a terminal after a restart (until PTY hosts exist).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// Saved-command name, or None for a plain shell.
    pub shortcut: Option<String>,
    pub cwd: Option<String>,
    /// Detached session id, when the shell lives in a PTY host.
    #[serde(default)]
    pub session: Option<String>,
}

pub type GroupId = u64;

/// Padding between a group's frame and its members when created.
pub const GROUP_PAD: f32 = 18.0;
/// Height of the name tab drawn above a group's frame (world units).
pub const GROUP_LABEL_H: f32 = 22.0;
/// Smallest group frame.
pub const GROUP_MIN: f32 = 80.0;

/// A named, tinted frame around a set of items. Dragging the frame moves
/// the members; membership is by containment in `rect`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    /// Content box in world units (the label sits above it).
    pub rect: WRect,
    /// ANSI palette index used for the tint.
    pub color: u8,
    pub members: Vec<ItemId>,
}

/// Part of a group frame under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GroupPart {
    Label,
    Edge(Edge),
    Inside,
}

/// Hit-test a group's screen-space content box and label.
pub fn group_part(screen: WRect, label: WRect, sx: f32, sy: f32) -> Option<GroupPart> {
    if label.contains(sx, sy) {
        return Some(GroupPart::Label);
    }
    let e = EDGE_PX;
    let outer = WRect::new(screen.x - e, screen.y - e, screen.w + 2.0 * e, screen.h + 2.0 * e);
    if !outer.contains(sx, sy) {
        return None;
    }
    let left = sx < screen.x + e;
    let right = sx >= screen.right() - e;
    let top = sy < screen.y + e;
    let bottom = sy >= screen.bottom() - e;
    if left || right || top || bottom {
        return Some(GroupPart::Edge(Edge { left, right, top, bottom }));
    }
    Some(GroupPart::Inside)
}

/// How a canvas tab presents itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CanvasMode {
    /// One terminal filling the window, no frame: the classic tab.
    #[default]
    Single,
    /// Free layout on an infinite, zoomable surface.
    Free,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Canvas {
    pub id: CanvasId,
    pub name: String,
    #[serde(default)]
    pub mode: CanvasMode,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub view: Viewport,
    pub items: Vec<Item>,
    /// Focused item.
    #[serde(default)]
    pub focus: Option<ItemId>,
    /// Viewport to restore when Focus Mode is toggled off.
    #[serde(default)]
    pub focus_prev: Option<Viewport>,
    /// Counter for cascading new items.
    #[serde(default)]
    pub spawn_n: u32,
    /// Group frames, drawn behind items.
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Multi-selection (the focused item may or may not be in it).
    #[serde(skip)]
    pub selected: Vec<ItemId>,
    /// Group whose label was clicked last; Focus Mode zooms to it.
    #[serde(skip)]
    pub group_sel: Option<GroupId>,
}

impl Canvas {
    pub fn new(id: CanvasId, name: impl Into<String>) -> Self {
        Self { id, name: name.into(), mode: CanvasMode::Free, cwd: None, view: Viewport::default(), items: Vec::new(), focus: None, focus_prev: None, spawn_n: 0, groups: Vec::new(), selected: Vec::new(), group_sel: None }
    }

    /// A classic terminal tab: one maximized item.
    pub fn single(id: CanvasId, item: Item) -> Self {
        let focus = Some(item.id);
        Self { id, name: String::new(), mode: CanvasMode::Single, cwd: None, view: Viewport::default(), items: vec![item], focus, focus_prev: None, spawn_n: 0, groups: Vec::new(), selected: Vec::new(), group_sel: None }
    }

    pub fn is_single(&self) -> bool {
        self.mode == CanvasMode::Single
    }

    /// Terminal ids on this canvas, in draw order, each once (mirrors
    /// share their terminal with the item that owns it).
    pub fn tabs(&self) -> impl Iterator<Item = TabId> + '_ {
        let mut seen: Vec<TabId> = Vec::new();
        self.items.iter().filter_map(move |i| match i.kind {
            ItemKind::Terminal(t) if !seen.contains(&t) => {
                seen.push(t);
                Some(t)
            }
            _ => None,
        })
    }

    /// Every item showing terminal `tab` (owner first, then mirrors).
    pub fn items_for_tab(&self, tab: TabId) -> Vec<ItemId> {
        let mut v: Vec<ItemId> = self.items.iter().filter(|i| matches!(i.kind, ItemKind::Terminal(t) if t == tab)).map(|i| i.id).collect();
        v.sort_by_key(|id| self.item(*id).map(|i| i.mirror).unwrap_or(false));
        v
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn item_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    /// The item that owns terminal `tab` (not a mirror), else any mirror.
    pub fn item_for_tab(&self, tab: TabId) -> Option<&Item> {
        self.items
            .iter()
            .find(|i| !i.mirror && matches!(i.kind, ItemKind::Terminal(t) if t == tab))
            .or_else(|| self.items.iter().find(|i| matches!(i.kind, ItemKind::Terminal(t) if t == tab)))
    }

    /// Bring an item to the front (drawn last, hit first).
    pub fn raise(&mut self, id: ItemId) {
        if let Some(pos) = self.items.iter().position(|i| i.id == id) {
            let it = self.items.remove(pos);
            self.items.push(it);
        }
    }

    // --- groups and selection ---------------------------------------------

    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn group_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// Recompute every group's members by containment, and drop members
    /// that no longer exist. An item belongs to at most one group: the
    /// smallest frame that contains it wins.
    pub fn recompute_membership(&mut self) {
        let mut order: Vec<usize> = (0..self.groups.len()).collect();
        order.sort_by(|&a, &b| {
            let (ra, rb) = (self.groups[a].rect, self.groups[b].rect);
            (ra.w * ra.h).partial_cmp(&(rb.w * rb.h)).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut taken: Vec<ItemId> = Vec::new();
        for gi in order {
            let rect = self.groups[gi].rect;
            let members: Vec<ItemId> = self
                .items
                .iter()
                .filter(|i| i.pin.is_none() && !taken.contains(&i.id) && rect.contains_rect(&i.rect))
                .map(|i| i.id)
                .collect();
            taken.extend(members.iter().copied());
            self.groups[gi].members = members;
        }
    }

    /// Is the item part of the current multi-selection?
    pub fn is_selected(&self, id: ItemId) -> bool {
        self.selected.contains(&id)
    }

    /// The focused item plus any multi-selection, deduplicated.
    pub fn selection_or_focus(&self) -> Vec<ItemId> {
        let mut v = self.selected.clone();
        if let Some(f) = self.focus
            && !v.contains(&f)
            && self.item(f).is_some()
        {
            v.push(f);
        }
        v.retain(|id| self.item(*id).is_some());
        v
    }

    /// Union of the given items' rects.
    pub fn bounds_of(&self, ids: &[ItemId]) -> Option<WRect> {
        let mut it = ids.iter().filter_map(|id| self.item(*id)).filter(|i| i.pin.is_none()).map(|i| i.rect);
        let first = it.next()?;
        Some(it.fold(first, |a, b| a.union(&b)))
    }

    /// Bounding rect of all items (None if empty).
    pub fn bounds(&self) -> Option<WRect> {
        let mut it = self.items.iter().filter(|i| i.pin.is_none()).map(|i| i.rect);
        let first = it.next()?;
        Some(it.fold(first, |a, b| a.union(&b)))
    }

    /// A free-ish spot for a new item of the given size: the visible
    /// centre, cascaded a little for each successive spawn.
    pub fn spawn_rect(&mut self, area: WRect, w: f32, h: f32) -> WRect {
        const GAP: f32 = 24.0;
        let vis = self.view.visible(area);
        // Beside the focused (else last) item when there is one, otherwise
        // centred in view. `spawn_n` keeps repeated spawns from stacking.
        let anchor = self.focus.and_then(|f| self.item(f)).filter(|i| i.pin.is_none()).or(self.items.iter().rev().find(|i| i.pin.is_none())).map(|i| i.rect);
        let r = match anchor {
            Some(a) => {
                let mut r = WRect::new(a.right() + GAP, a.y, w, h);
                // Try right, then below, then a cascade offset.
                let busy = |r: &WRect| self.items.iter().any(|i| i.pin.is_none() && i.rect.intersects(r));
                if busy(&r) {
                    r = WRect::new(a.x, a.bottom() + GAP, w, h);
                }
                if busy(&r) {
                    let n = (self.spawn_n % 6 + 1) as f32;
                    r = WRect::new(a.x + n * 40.0, a.y + n * 32.0, w, h);
                }
                r
            }
            None => WRect::new(vis.x + (vis.w - w) / 2.0, vis.y + (vis.h - h) / 2.0, w, h),
        };
        self.spawn_n += 1;
        WRect::new(r.x.round(), r.y.round(), w, h)
    }

    /// Snap an item rect's edges to other items' edges within `tol` world
    /// units. Returns the adjusted rect.
    pub fn snap(&self, id: ItemId, mut r: WRect, tol: f32, resize_edges: bool) -> WRect {
        let mut best_dx: Option<f32> = None;
        let mut best_dy: Option<f32> = None;
        let consider = |best: &mut Option<f32>, d: f32| {
            if d.abs() <= tol && best.map(|b| d.abs() < b.abs()).unwrap_or(true) {
                *best = Some(d);
            }
        };
        for o in self.items.iter().filter(|o| o.id != id && o.pin.is_none()) {
            let xs = [o.rect.x, o.rect.right()];
            let ys = [o.rect.y, o.rect.bottom()];
            for &ox in &xs {
                if resize_edges {
                    consider(&mut best_dx, ox - r.right());
                } else {
                    consider(&mut best_dx, ox - r.x);
                    consider(&mut best_dx, ox - r.right());
                }
            }
            for &oy in &ys {
                if resize_edges {
                    consider(&mut best_dy, oy - r.bottom());
                } else {
                    consider(&mut best_dy, oy - r.y);
                    consider(&mut best_dy, oy - r.bottom());
                }
            }
        }
        if let Some(dx) = best_dx {
            if resize_edges { r.w += dx } else { r.x += dx }
        }
        if let Some(dy) = best_dy {
            if resize_edges { r.h += dy } else { r.y += dy }
        }
        r
    }
}

/// Which part of an item the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemPart {
    Title,
    Close,
    Content,
    /// Resize handle; the two bools are (right edge, bottom edge); false
    /// means the opposite edge. Corners set both flags meaningfully via
    /// `Edge`.
    Edge(Edge),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Classify a screen point against an item's screen rect.
pub fn item_part(screen: WRect, title_h: f32, sx: f32, sy: f32, close_w: f32) -> Option<ItemPart> {
    let e = EDGE_PX;
    let outer = WRect::new(screen.x - e, screen.y - e, screen.w + 2.0 * e, screen.h + 2.0 * e);
    if !outer.contains(sx, sy) {
        return None;
    }
    let left = sx < screen.x + e;
    let right = sx >= screen.right() - e;
    let top = sy < screen.y + e;
    let bottom = sy >= screen.bottom() - e;
    if left || right || top || bottom {
        return Some(ItemPart::Edge(Edge { left, right, top, bottom }));
    }
    if sy < screen.y + title_h {
        if sx >= screen.right() - close_w {
            return Some(ItemPart::Close);
        }
        return Some(ItemPart::Title);
    }
    Some(ItemPart::Content)
}

/// Persisted state of the whole app (all windows).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavedState {
    pub version: u32,
    pub windows: Vec<SavedWindow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedWindow {
    pub width: u32,
    pub height: u32,
    /// Tabs, in tab-bar order.
    pub canvases: Vec<Canvas>,
    pub active: usize,
}

impl SavedState {
    pub fn path() -> std::path::PathBuf {
        crate::config::config_dir().join("state.json")
    }

    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(Self::path()).ok()?;
        match serde_json::from_str::<Self>(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("state.json unreadable ({e}); starting fresh");
                None
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
