//! Live configuration: watch `~/.config/kindlyterm/*.toml` and re-apply
//! changes through the same paths the Deck uses, so a hand edit takes
//! effect without a restart.

use super::*;
use notify::{RecursiveMode, Watcher};

/// Start watching the config directory. Returns the watcher (drop it to
/// stop) or None if inotify is unavailable.
pub(super) fn start_config_watcher(proxy: EventLoopProxy<UserEvent>) -> Option<notify::RecommendedWatcher> {
    let dir = Config::path().parent()?.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let interesting = ev.paths.iter().any(|p| {
                matches!(p.file_name().and_then(|n| n.to_str()), Some("config.toml" | "effects.toml" | "commands.toml"))
            });
            if interesting && (ev.kind.is_modify() || ev.kind.is_create() || ev.kind.is_remove()) {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;
    watcher.watch(&dir, RecursiveMode::NonRecursive).ok()?;
    // Editors write in bursts (temp file, rename, chmod): coalesce them.
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(250));
                while rx.try_recv().is_ok() {}
                let _ = proxy.send_event(UserEvent { tab: crate::terminal::SYS_CONFIG_CHANGED, event: Event::Wakeup });
            }
        })
        .ok()?;
    log::info!("watching {} for changes", dir.display());
    Some(watcher)
}

impl App {
    /// Re-read every config file and apply what differs.
    pub(super) fn reload_config_files(&mut self) {
        let mut changed: Vec<&str> = Vec::new();
        let new = Config::load();
        if new != self.config {
            let old = std::mem::replace(&mut self.config, new.clone());
            if old.font.family != new.font.family && !self.reload_font(&new.font.family) {
                self.config.font.family = old.font.family.clone();
            }
            if (old.font.size - new.font.size).abs() > 0.01 {
                self.set_font_pt(new.font.size);
            }
            if (old.font.line_padding - new.font.line_padding).abs() > 0.01 {
                let (pt, lp) = (self.font_pt, new.font.line_padding);
                for w in &mut self.wins {
                    w.fonts.set_size(pt * w.scale as f32, lp * w.scale as f32);
                }
                self.relayout_all();
            }
            if old.colors != new.colors {
                self.theme = Theme::from_config(&self.config.colors);
                self.preview_colors = None;
            }
            if (old.terminal.padding - new.terminal.padding).abs() > 0.01 {
                self.relayout_all();
            }
            if old.terminal.cursor != new.terminal.cursor || old.terminal.scrollback != new.terminal.scrollback {
                self.apply_term_options();
            }
            changed.push("config.toml");
        }
        let fx = EffectsConfig::load();
        if fx != self.effects {
            self.effects = fx;
            changed.push("effects.toml");
        }
        let store = CommandStore::load();
        if store.commands.len() != self.store.commands.len() || store.commands.iter().zip(&self.store.commands).any(|(a, b)| a != b) {
            self.store = store;
            changed.push("commands.toml");
        }
        if !changed.is_empty() {
            log::info!("reloaded {}", changed.join(", "));
            self.set_status(format!("reloaded {}", changed.join(" · ")));
            self.request_redraw_all();
        }
    }
}
