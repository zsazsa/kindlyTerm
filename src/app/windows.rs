//! Window lifecycle: creation, closing, moving tabs between windows,
//! drag-and-drop resolution, and animation wake-up scheduling.

use super::*;

impl App {
    /// Create a new top-level window holding `tabs` (may be empty, in which
    /// case a shell tab is opened). Returns its index in `wins`.
    pub(super) fn create_window(&mut self, event_loop: &ActiveEventLoop, tabs: Vec<Terminal>) -> Option<usize> {
        // The app id / WM_CLASS must match the .desktop file name so GNOME
        // pairs the window with its launcher entry and icon.
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("kindterm")
            .with_transparent(true)
            .with_inner_size(LogicalSize::new(1100.0, 720.0));
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = attrs.with_name("kindterm", "kindterm");
        }
        {
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = attrs.with_name("kindterm", "kindterm");
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                return None;
            }
        };
        let scale = window.scale_factor();
        let gpu = match &self.gpu {
            Some(g) => Arc::clone(g),
            None => match Gpu::new(&window) {
                Ok(g) => {
                    self.gpu = Some(Arc::clone(&g));
                    g
                }
                Err(e) => {
                    log::error!("GPU init failed: {e:#}");
                    return None;
                }
            },
        };
        let renderer = match Renderer::new(gpu.clone(), window.clone()) {
            Ok(r) => r,
            Err(e) => {
                log::error!("renderer init failed: {e:#}");
                return None;
            }
        };
        let family = self.wins.first().map(|w| w.fonts.family.clone()).unwrap_or_else(|| self.config.font.family.clone());
        let fonts = match FontSystem::new(&family, self.font_pt * scale as f32, self.config.font.line_padding * scale as f32) {
            Ok(f) => f,
            Err(e) => {
                log::error!("font init failed: {e:#}");
                return None;
            }
        };
        if self.gpu_name.is_empty() {
            self.gpu_name = gpu.adapter_name.clone();
        }
        if self.font_families.is_empty() {
            self.font_families = fonts.monospace_families();
        }
        self.wins.push(Win {
            window,
            renderer,
            fonts,
            batch: Batch::default(),
            layout: None,
            scale,
            tabs,
            active: 0,
            palette: None,
            menu: None,
            tab_hits: Vec::new(),
            plus_hit: None,
            hover: Hover::None,
            mouse: PhysicalPosition::new(0.0, 0.0),
            selecting: false,
            last_click: None,
            click_count: 0,
            focused: true,
            status: None,
            frame: 0,
            deck: Deck::new(),
            drag: None,
            rename: None,
            last_tab_click: None,
            cursor_anim: CursorAnim::new(),
            fx: Effects::new(),
            cheat: false,
        });
        let wi = self.wins.len() - 1;
        self.cur = wi;
        self.relayout_win(wi);
        if self.wins[wi].tabs.is_empty() {
            let launch = self.shell_launch();
            self.open_tab(launch);
        } else {
            self.update_window_title();
        }
        Some(wi)
    }

    /// Close a window, shutting down its terminals. Exits when none remain.
    pub(super) fn close_window(&mut self, wi: usize, event_loop: &ActiveEventLoop) {
        if wi >= self.wins.len() {
            return;
        }
        let w = self.wins.remove(wi);
        for t in &w.tabs {
            t.shutdown();
        }
        drop(w);
        log::info!("closed window {wi}; {} left", self.wins.len());
        if self.wins.is_empty() {
            log::info!("last window closed: exiting");
            event_loop.exit();
            return;
        }
        if self.cur >= self.wins.len() {
            self.cur = self.wins.len() - 1;
        }
    }

    pub(super) fn window_index(&self, id: WindowId) -> Option<usize> {
        self.wins.iter().position(|w| w.window.id() == id)
    }

    pub(super) fn window_of_tab(&self, tab: TabId) -> Option<usize> {
        self.wins.iter().position(|w| w.tabs.iter().any(|t| t.id == tab))
    }

    /// Take a tab out of window `from` (by tab id).
    pub(super) fn detach_tab(&mut self, from: usize, tab: TabId, event_loop: &ActiveEventLoop) -> Option<Terminal> {
        let w = self.wins.get_mut(from)?;
        let idx = w.tabs.iter().position(|t| t.id == tab)?;
        let term = w.tabs.remove(idx);
        w.drag = None;
        if w.tabs.is_empty() {
            self.close_window(from, event_loop);
        } else {
            if w.active >= w.tabs.len() {
                w.active = w.tabs.len() - 1;
            } else if idx < w.active {
                w.active -= 1;
            }
            let saved = self.cur;
            self.cur = from;
            self.update_window_title();
            self.request_redraw();
            self.cur = saved.min(self.wins.len().saturating_sub(1));
        }
        Some(term)
    }

    /// Put a detached tab into window `to` and make it active.
    pub(super) fn attach_tab(&mut self, to: usize, mut term: Terminal) {
        if to >= self.wins.len() {
            return;
        }
        let size = Self::grid_size_of(&self.wins[to]);
        term.resize(size);
        let w = &mut self.wins[to];
        w.tabs.push(term);
        w.active = w.tabs.len() - 1;
        let saved = self.cur;
        self.cur = to;
        self.update_window_title();
        self.request_redraw();
        self.cur = saved;
    }

    /// Move a tab into a brand-new window.
    pub(super) fn tear_off(&mut self, from: usize, tab: TabId, event_loop: &ActiveEventLoop) {
        // A window with a single tab is already "its own window".
        if self.wins.get(from).map(|w| w.tabs.len() <= 1).unwrap_or(true) {
            return;
        }
        let Some(term) = self.detach_tab(from, tab, event_loop) else { return };
        let title = term.display_title().to_string();
        match self.create_window(event_loop, vec![term]) {
            Some(wi) => {
                log::info!("tear-off: '{title}' -> new window ({} windows)", self.wins.len());
                self.wins[wi].status = Some((format!("{title} → new window"), Instant::now()));
            }
            None => log::error!("tear-off failed: could not create window"),
        }
    }

    /// Move a tab from window `from` into window `to`.
    pub(super) fn move_tab_to_window(&mut self, from: usize, tab: TabId, to: usize, event_loop: &ActiveEventLoop) {
        if from == to {
            return;
        }
        let to_id = self.wins.get(to).map(|w| w.window.id());
        let Some(term) = self.detach_tab(from, tab, event_loop) else { return };
        // Indices may have shifted if `from` closed.
        let Some(to) = to_id.and_then(|id| self.window_index(id)) else { return };
        let title = term.display_title().to_string();
        self.attach_tab(to, term);
        log::info!("merge: '{title}' moved into window {to} ({} windows)", self.wins.len());
        self.wins[to].status = Some((format!("{title} moved here"), Instant::now()));
        self.wins[to].window.focus_window();
    }

    /// Called when a dragged tab is released outside its window.
    pub(super) fn start_pending_drop(&mut self, tab: TabId) {
        let from = self.win().window.id();
        log::debug!("pending drop: tab {tab} released outside window {from:?}");
        self.pending_drop = Some(PendingDrop { from, tab, at: Instant::now() });
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(900));
            let _ = proxy.send_event(UserEvent { tab: crate::terminal::SYS_DROP_TIMEOUT, event: Event::Wakeup });
        });
    }

    pub(super) fn resolve_pending_drop(&mut self, entered: Option<WindowId>, event_loop: &ActiveEventLoop) {
        let Some(p) = self.pending_drop.take() else { return };
        log::debug!("resolve drop: entered {entered:?} from {:?} after {:?}", p.from, p.at.elapsed());
        let Some(from) = self.window_index(p.from) else { return };
        match entered {
            Some(id) if id != p.from => {
                if let Some(to) = self.window_index(id) {
                    self.move_tab_to_window(from, p.tab, to, event_loop);
                }
            }
            _ => {
                if p.at.elapsed().as_millis() < 2000 {
                    self.tear_off(from, p.tab, event_loop);
                }
            }
        }
    }
}

impl App {
    /// Does this window need animation frames right now?
    pub(super) fn window_animating(w: &Win, anim: &str) -> bool {
        if anim == "none" {
            return false;
        }
        if w.cursor_anim.transient() || w.fx.active() {
            return true;
        }
        // Resting breath/blink only while focused and the app shows a cursor.
        w.focused
    }

    /// Pick the next wake-up: animation frame or status expiry.
    pub(super) fn schedule_wakeups(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let anim = self.config.terminal.cursor_animation.clone();
        let mut next: Option<Instant> = None;
        let consider = |t: Instant, next: &mut Option<Instant>| {
            *next = Some(next.map(|n| n.min(t)).unwrap_or(t));
        };
        for w in &self.wins {
            if let Some((_, at)) = w.status.as_ref() {
                consider(*at + std::time::Duration::from_secs(4), &mut next);
            }
            if Self::window_animating(w, &anim) {
                let ms = if w.cursor_anim.transient() || w.fx.active() { 16 } else { 40 };
                let t = now + std::time::Duration::from_millis(ms);
                consider(t, &mut next);
                self.next_frame = Some(self.next_frame.map(|n| n.min(t)).unwrap_or(t));
            }
        }
        match next {
            Some(t) => event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait),
        }
    }
}
