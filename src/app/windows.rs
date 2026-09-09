//! Window lifecycle: creation, closing, moving tabs between windows,
//! drag-and-drop resolution, and animation wake-up scheduling.

use super::*;

impl App {
    /// Create a new top-level window holding `tabs` (may be empty, in which
    /// case a shell tab is opened). Returns its index in `wins`.
    pub(super) fn create_window(&mut self, event_loop: &ActiveEventLoop, bundle: Option<(Canvas, Vec<Terminal>)>) -> Option<usize> {
        // The app id / WM_CLASS must match the .desktop file name so GNOME
        // pairs the window with its launcher entry and icon.
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("kindlyTerm")
            .with_transparent(true)
            .with_inner_size(LogicalSize::new(1100.0, 720.0));
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = attrs.with_name("kindlyterm", "kindlyterm");
        }
        {
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = attrs.with_name("kindlyterm", "kindlyterm");
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
            terms: Vec::new(),
            canvases: Vec::new(),
            active: 0,
            cdrag: CDrag::None,
            hover_part: None,
            space_held: false,
            pan_mode: false,
            last_ctrl_release: None,
            last_title_click: None,
            dirty: false,
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
            cheat: false,
        });
        let wi = self.wins.len() - 1;
        self.cur = wi;
        self.relayout_win(wi);
        match bundle {
            Some((canvas, terms)) => {
                let w = &mut self.wins[wi];
                w.terms = terms;
                w.canvases.push(canvas);
                w.active = 0;
                w.dirty = true;
                self.relayout_win(wi);
                self.update_window_title();
            }
            None => {
                let launch = self.shell_launch();
                self.open_tab(launch);
            }
        }
        Some(wi)
    }

    /// Close a window, shutting down its terminals. Exits when none remain.
    pub(super) fn close_window(&mut self, wi: usize, event_loop: &ActiveEventLoop) {
        if wi >= self.wins.len() {
            return;
        }
        // Persist before the last window disappears so it comes back next time.
        if self.wins.len() == 1 {
            self.save_state();
        }
        let w = self.wins.remove(wi);
        for t in &w.terms {
            t.shutdown();
        }
        drop(w);
        if !self.wins.is_empty() {
            self.save_state();
        }
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
        self.wins.iter().position(|w| w.terms.iter().any(|t| t.id == tab))
    }

    /// Take tab (canvas) `index` out of window `from`, with its terminals.
    pub(super) fn detach_canvas(&mut self, from: usize, index: usize, event_loop: &ActiveEventLoop) -> Option<(Canvas, Vec<Terminal>)> {
        let w = self.wins.get_mut(from)?;
        if index >= w.canvases.len() {
            return None;
        }
        let canvas = w.canvases.remove(index);
        let mut terms = Vec::new();
        for t in canvas.tabs() {
            if let Some(i) = w.term_index(t) {
                terms.push(w.terms.remove(i));
            }
        }
        w.drag = None;
        w.dirty = true;
        if w.canvases.is_empty() {
            self.close_window(from, event_loop);
        } else {
            if w.active >= w.canvases.len() {
                w.active = w.canvases.len() - 1;
            } else if index < w.active {
                w.active -= 1;
            }
            let saved = self.cur;
            self.cur = from;
            self.update_window_title();
            self.request_redraw();
            self.cur = saved.min(self.wins.len().saturating_sub(1));
        }
        Some((canvas, terms))
    }

    /// Put a detached tab into window `to` and make it active.
    pub(super) fn attach_canvas(&mut self, to: usize, canvas: Canvas, terms: Vec<Terminal>) {
        if to >= self.wins.len() {
            return;
        }
        let w = &mut self.wins[to];
        w.terms.extend(terms);
        w.canvases.push(canvas);
        w.active = w.canvases.len() - 1;
        w.dirty = true;
        let saved = self.cur;
        self.cur = to;
        self.relayout_win(to);
        self.update_window_title();
        self.request_redraw();
        self.cur = saved;
    }

    /// Move a tab into a brand-new window.
    pub(super) fn tear_off(&mut self, from: usize, index: usize, event_loop: &ActiveEventLoop) {
        // A window with a single tab is already "its own window".
        if self.wins.get(from).map(|w| w.canvases.len() <= 1).unwrap_or(true) {
            return;
        }
        let title = self.wins[from].tab_title(index);
        let Some(bundle) = self.detach_canvas(from, index, event_loop) else { return };
        match self.create_window(event_loop, Some(bundle)) {
            Some(wi) => {
                log::info!("tear-off: '{title}' -> new window ({} windows)", self.wins.len());
                self.wins[wi].status = Some((format!("{title} → new window"), Instant::now()));
            }
            None => log::error!("tear-off failed: could not create window"),
        }
    }

    /// Move tab `index` from window `from` into window `to`.
    pub(super) fn move_tab_to_window(&mut self, from: usize, index: usize, to: usize, event_loop: &ActiveEventLoop) {
        if from == to {
            return;
        }
        let to_id = self.wins.get(to).map(|w| w.window.id());
        let title = self.wins.get(from).map(|w| w.tab_title(index)).unwrap_or_default();
        let Some(bundle) = self.detach_canvas(from, index, event_loop) else { return };
        // Indices may have shifted if `from` closed.
        let Some(to) = to_id.and_then(|id| self.window_index(id)) else { return };
        self.attach_canvas(to, bundle.0, bundle.1);
        log::info!("merge: '{title}' moved into window {to} ({} windows)", self.wins.len());
        self.wins[to].status = Some((format!("{title} moved here"), Instant::now()));
    }

    /// Called when a dragged tab is released outside its window.
    pub(super) fn start_pending_drop(&mut self, canvas: CanvasId) {
        let from = self.win().window.id();
        log::debug!("pending drop: tab {canvas} released outside window {from:?}");
        self.pending_drop = Some(PendingDrop { from, tab: canvas, at: Instant::now() });
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
        let Some(index) = self.wins[from].canvases.iter().position(|c| c.id == p.tab) else { return };
        match entered {
            Some(id) if id != p.from => {
                if let Some(to) = self.window_index(id) {
                    self.move_tab_to_window(from, index, to, event_loop);
                }
            }
            _ => {
                if p.at.elapsed().as_millis() < 2000 {
                    self.tear_off(from, index, event_loop);
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
        if w.any_view_transient() {
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
                let ms = if w.any_view_transient() { 16 } else { 40 };
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
