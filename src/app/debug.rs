//! Developer hooks (screenshots, scripted input). Inert unless
//! KINDLYTERM_DEBUG=1 is set in the environment.

use super::*;

impl App {
    pub(super) fn take_debug_screenshot(&mut self) {
        let Some(l) = self.wins[self.cur].layout else { return };
        if self.debug.shot_done {
            return;
        }
        self.wins[self.cur].batch.clear();
        self.draw_terminal(l);
        self.draw_deck(l);
        self.draw_tab_bar(l);
        self.draw_palette(l);
        self.draw_menu(l);
        self.draw_cheat(l);
        let Some(path) = self.debug.screenshot.clone() else { return };
        let bg = with_alpha(self.theme.bg, self.config.colors.opacity.clamp(0.3, 1.0));
        let w = &mut self.wins[self.cur];
        match w.renderer.screenshot(&mut w.fonts, &w.batch, bg, &path) {
            Ok(()) => log::info!("wrote screenshot {}", path.display()),
            Err(e) => log::error!("screenshot failed: {e:#}"),
        }
        self.debug.shot_done = true;
    }

    pub(super) fn run_debug_actions(&mut self, event_loop: Option<&ActiveEventLoop>) {
        {
            let mut actions = std::mem::take(&mut self.debug.actions).into_iter();
            while let Some(a) = actions.next() {
                if let Some(("wait", ms)) = a.split_once(':') {
                    // Yield to the event loop; resume the rest after `ms`.
                    self.debug.actions = actions.collect();
                    let ms: u64 = ms.parse().unwrap_or(300);
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                        let _ = proxy.send_event(UserEvent { tab: 0, event: Event::Wakeup });
                    });
                    break;
                }
                match a.split_once(':') {
                    Some(("scroll", n)) => self.scroll_active(Scroll::Delta(n.parse().unwrap_or(0))),
                    _ if a == "palette" => self.wins[self.cur].deck.open(PageId::Home),
                    Some(("deck", page)) => {
                        let id = match page {
                            "theme" => PageId::Theme,
                            "font" => PageId::Font,
                            "manage" => PageId::Manage,
                            "clipboard" => PageId::Clipboard,
                            "keyboard" => PageId::Keyboard,
                            "cursor" => PageId::Cursor,
                            "about" => PageId::About,
                            "effects" => PageId::Effects,
                            "opacity" => PageId::Opacity,
                            "padding" => PageId::Padding,
                            "shell" => PageId::Shell,
                            "scrollback" => PageId::Scrollback,
                            "tabs" => PageId::Tabs,
                            "editor" => {
                                self.open_shortcut_editor();
                                continue;
                            }
                            _ => PageId::Home,
                        };
                        self.wins[self.cur].deck.open(id);
                    }
                    Some(("type", text)) => {
                        // Feed tokens ("gra|down|enter") through the deck.
                        for tok in text.split('|') {
                            let action = {
                                let fam = self.win().fonts.family.clone();
                let env = deck_env!(self, fam);
                                self.wins[self.cur].deck.debug_type(tok, &env)
                            };
                            self.apply_deck_action(action);
                        }
                    }
                    _ if a == "tabs" => self.open_palette(Mode::Tabs),
                    _ if a == "save" => self.open_palette(Mode::SaveCommand { command: None }),
                    _ if a == "menu:tabbar" => self.open_tab_bar_menu(60.0, 14.0),
                    _ if a == "menu:term" => self.open_terminal_menu(300.0, 200.0),
                    _ if a == "hover:close" => self.wins[self.cur].hover = Hover::Close(0),
                    Some(("clipset", text)) => {
                        let r = self.clipboard.as_mut().map(|c| c.set_text(text.to_string()));
                        log::info!("clipset {text:?}: {r:?}");
                    }
                    _ if a == "clipget" => {
                        let r = self.clipboard.as_mut().map(|c| c.get_text());
                        log::info!("clipget: {r:?}");
                    }
                    _ if a == "clippaste" => self.paste(),
                    Some(("resize", wh)) => {
                        if let Some((w, h)) = wh.split_once('x')
                            && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                                let _ = self.win().window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
                            }
                    }
                    Some(("reflow", n)) => {
                        // Measure grid reflow: shrink columns by n, then restore.
                        let base = self.grid_size();
                        let n: usize = n.parse().unwrap_or(10);
                        let t0 = Instant::now();
                        for step in 1..=n {
                            let mut g = base;
                            g.cols = base.cols.saturating_sub(step).max(2);
                            for tab in &mut self.wins[self.cur].tabs {
                                tab.resize(g);
                            }
                        }
                        for tab in &mut self.wins[self.cur].tabs {
                            tab.resize(base);
                        }
                        let dt = t0.elapsed().as_secs_f64() * 1e3;
                        log::info!("reflow: {} column changes x {} tabs in {:.1}ms ({:.2}ms each)", n + 1, self.wins[self.cur].tabs.len(), dt, dt / (n + 1) as f64);
                    }
                    Some(("mouse", rest)) => {
                        // mouse:down:X:Y | mouse:move:X:Y | mouse:up:X:Y | mouse:right:X:Y
                        let parts: Vec<&str> = rest.split(':').collect();
                        if let (Some(kind), Some(x), Some(y), Some(el)) = (parts.first(), parts.get(1), parts.get(2), event_loop) {
                            let (x, y): (f64, f64) = (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                            self.win_mut().mouse = PhysicalPosition::new(x, y);
                            match *kind {
                                "down" => self.on_mouse_button(ElementState::Pressed, MouseButton::Left, el),
                                "up" => self.on_mouse_button(ElementState::Released, MouseButton::Left, el),
                                "right" => self.on_mouse_button(ElementState::Pressed, MouseButton::Right, el),
                                _ => self.on_mouse_move(),
                            }
                        }
                    }
                    _ if a == "enter" => {
                        // Simulate the pointer entering this window (drop target)
                        // through the real window-event path.
                        if let Some(el) = event_loop {
                            let id = self.win().window.id();
                            self.window_event(el, id, WindowEvent::CursorEntered { device_id: winit::event::DeviceId::dummy() });
                            self.window_event(el, id, WindowEvent::CursorMoved { device_id: winit::event::DeviceId::dummy(), position: PhysicalPosition::new(200.0, 200.0) });
                            self.window_event(el, id, WindowEvent::Focused(true));
                            self.window_event(el, id, WindowEvent::RedrawRequested);
                        }
                    }
                    Some(("renametext", text)) => {
                        if let Some((_, t, all)) = self.win_mut().rename.as_mut() {
                            *t = text.to_string();
                            *all = false;
                        }
                    }
                    _ if a == "renamecommit" => self.end_rename(true),
                    Some(("chomp", dir)) => {
                        let now = Instant::now();
                        let left = dir != "right";
                        self.win_mut().cursor_anim.chomp = Some((now - std::time::Duration::from_millis(1600), now, left));
                    }
                    _ if a == "cheat" => self.win_mut().cheat = true,
                    _ if a == "shot" => {
                        self.debug.shot_done = false;
                        self.take_debug_screenshot();
                    }
                    Some(("frames", spec)) => {
                        // frames:N:MS -> N numbered screenshots MS apart (for GIFs).
                        let (n, ms) = spec.split_once(':').map(|(n, m)| (n.parse().unwrap_or(10), m.parse().unwrap_or(40))).unwrap_or((10u32, 40u64));
                        let base = self.debug.screenshot.clone();
                        for _ in 0..n {
                            if let Some(base) = base.as_ref() {
                                let stem = base.with_extension("");
                                let i = self.debug.frame_seq;
                                self.debug.frame_seq += 1;
                                self.debug.screenshot = Some(std::path::PathBuf::from(format!("{}_{i:03}.png", stem.display())));
                                self.debug.shot_done = false;
                                self.take_debug_screenshot();
                            }
                            std::thread::sleep(std::time::Duration::from_millis(ms));
                        }
                        self.debug.screenshot = base;
                        self.debug.shot_done = true;
                    }
                    Some(("typefast", text)) => {
                        // Simulate fast typing: trail glyphs + pty input.
                        let (x, y) = self.win().cursor_anim.to;
                        let cfg = self.effects.typing_trail.clone();
                        let w = self.win_mut();
                        let cw = w.fonts.metrics.width;
                        for (i, ch) in text.chars().enumerate() {
                            w.fx.typed(&cfg, ch, x + i as f32 * cw, y);
                        }
                        if let Some(tab) = w.tabs.get(w.active) {
                            tab.write(text.to_string().into_bytes());
                        }
                    }
                    Some(("pastefile", path)) => {
                        if let Ok(t) = std::fs::read_to_string(path) {
                            self.paste_text(t);
                        }
                    }
                    Some(("paste", text)) => {
                        let t = text.replace("\\n", "\n");
                        self.paste_text(t);
                    }
                    Some(("input", text)) => {
                        let text = text.replace("\\r", "\r");
                        if let Some(tab) = self.win().tabs.get(self.win().active) {
                            tab.write(text.into_bytes());
                        }
                    }
                    Some(("wheel", dy)) => {
                        let dy: f32 = dy.parse().unwrap_or(0.0);
                        self.on_wheel(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy as f64)));
                    }
                    Some(("del", n)) => {
                        // N backspaces, keeping the Pac-Man hold alive.
                        let n: usize = n.parse().unwrap_or(1);
                        let now = Instant::now();
                        let anim = &mut self.win_mut().cursor_anim;
                        anim.chomp = Some(match anim.chomp {
                            Some((start, _, l)) => (start, now, l),
                            None => (now - std::time::Duration::from_millis(1500), now, true),
                        });
                        if let Some(tab) = self.win().tabs.get(self.win().active) {
                            tab.write(vec![0x7f; n]);
                        }
                    }
                    Some(("tab", n)) => {
                        let n: usize = n.parse().unwrap_or(0);
                        self.switch_tab(n);
                    }
                    Some(("focuswin", n)) => {
                        // Debug: route subsequent actions to window n.
                        let n: usize = n.parse().unwrap_or(0);
                        if n < self.wins.len() {
                            self.cur = n;
                        }
                    }
                    Some(("sleep", ms)) => std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap_or(0))),
                    _ if a == "redraw" => {}
                    _ if a == "newtab" => {
                        let l = self.shell_launch();
                        self.open_tab(l);
                    }
                    _ => log::warn!("unknown debug action {a:?}"),
                }
                if event_loop.is_some() && !self.wins.is_empty() {
                    let _ = self.draw();
                }
            }
        }
        self.request_redraw();
    }

}
