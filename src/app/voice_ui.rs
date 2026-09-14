//! Voice input glue: the Ctrl+Shift+M key, worker messages, typing the
//! text, spoken commands and navigation.
//!
//! A voice session belongs to the window it was started in and dictation
//! is pinned to the terminal that was focused at that moment. Moving
//! between cards or canvases keeps it going; switching to another window
//! or application, or closing the window, stops it. "Switch to <name>"
//! moves both the focus and the pin.

use super::*;
use crate::voice::{Voice, VoiceAction, VoiceMsg, VoiceState};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

impl App {
    /// The voice key went down. Tap: toggle listening. Hold: push-to-talk
    /// (the release in `voice_release` commits). A press while listening
    /// stops and commits.
    pub(super) fn voice_press(&mut self) {
        self.chord_armed = None;
        self.voice_key_down = Some(Instant::now());
        let needs_engine = self.voice.as_ref().map(|v| v.state == VoiceState::Error).unwrap_or(true);
        if needs_engine {
            let proxy = self.proxy.clone();
            let mut v = Voice::spawn(self.config.voice.clone(), move || {
                let _ = proxy.send_event(UserEvent { tab: crate::terminal::SYS_VOICE, event: alacritty_terminal::event::Event::Wakeup });
            });
            v.start_when_ready = true;
            self.voice = Some(v);
            self.voice_pin_here();
            self.set_status("Loading the voice model…".into());
            return;
        }
        let state = self.voice.as_ref().map(|v| v.state).expect("voice");
        match state {
            VoiceState::Loading => {
                // A second tap while the model loads cancels the first.
                let v = self.voice.as_mut().expect("voice");
                v.start_when_ready = !v.start_when_ready;
                if v.start_when_ready {
                    self.voice_pin_here();
                } else {
                    self.voice_owner = None;
                    self.voice_key_down = None;
                }
            }
            VoiceState::Idle => {
                self.voice.as_ref().expect("voice").start();
                self.voice_pin_here();
            }
            VoiceState::Listening | VoiceState::Transcribing => {
                self.voice.as_ref().expect("voice").stop(true);
                // This press ended the session; its release must not act.
                self.voice_key_down = None;
            }
            VoiceState::Error => {}
        }
        self.request_redraw();
    }

    /// Make the current window the session owner and pin dictation to its
    /// focused terminal.
    fn voice_pin_here(&mut self) {
        self.voice_owner = Some(self.win().window.id());
        self.voice_target = self.win().active_term().map(|t| t.id);
    }

    /// The voice key came up. After a long hold this is the end of
    /// push-to-talk; after a tap listening simply stays on.
    pub(super) fn voice_release(&mut self) {
        let Some(down) = self.voice_key_down.take() else { return };
        if down.elapsed().as_millis() as u64 >= self.config.voice.hold_ms
            && let Some(v) = self.voice.as_mut() {
                v.start_when_ready = false;
                v.stop(true);
                self.request_redraw();
            }
    }

    /// The current window lost focus: if it owns the voice session, stop
    /// listening. Speech still in progress is discarded, not typed.
    pub(super) fn voice_window_unfocused(&mut self) {
        let here = self.win().window.id();
        if self.voice_stop_if_owner(here) {
            self.set_status("Voice input stopped: the window lost focus".into());
        }
    }

    /// A window is closing: end its voice session, if any.
    pub(super) fn voice_window_closing(&mut self, id: winit::window::WindowId) {
        self.voice_stop_if_owner(id);
    }

    /// Stop an active or pending session owned by `id`. True if one was.
    fn voice_stop_if_owner(&mut self, id: winit::window::WindowId) -> bool {
        if self.voice_owner != Some(id) {
            return false;
        }
        let Some(v) = self.voice.as_mut() else { return false };
        let active = match v.state {
            VoiceState::Loading => v.start_when_ready,
            VoiceState::Listening | VoiceState::Transcribing => true,
            VoiceState::Idle | VoiceState::Error => false,
        };
        if !active {
            return false;
        }
        v.start_when_ready = false;
        v.stop(false);
        self.voice_key_down = None;
        self.request_redraw_all();
        true
    }

    /// Handle everything the voice worker reported.
    pub(super) fn drain_voice(&mut self) {
        let Some(v) = self.voice.as_mut() else { return };
        let mut texts = Vec::new();
        let mut error = None;
        for m in v.drain() {
            match m {
                VoiceMsg::Ready => {
                    if v.start_when_ready {
                        v.start_when_ready = false;
                        v.start();
                    }
                }
                VoiceMsg::Text(t) => texts.push(t),
                VoiceMsg::Error(e) => error = Some(e),
                _ => {}
            }
        }
        for t in texts {
            self.voice_handle_text(&t, false);
        }
        if let Some(e) = error {
            self.set_status(e);
        }
        self.request_redraw_all();
    }

    /// Act on one dictated utterance: a spoken command presses a key or
    /// switches tabs, a navigation phrase focuses a card, anything else is
    /// typed into the pinned terminal. With `adopt`, a missing session
    /// pins itself to the current window's focused terminal first (the
    /// control API uses this to test voice flows without a microphone).
    /// Returns a short description of what happened.
    pub(super) fn voice_handle_text(&mut self, text: &str, adopt: bool) -> String {
        if adopt && (self.voice_owner.is_none() || self.voice_target.is_none()) {
            self.voice_pin_here();
        }
        match crate::voice::command_action(text, &self.config.voice) {
            Some(VoiceAction::Key(bytes)) => {
                self.voice_send(bytes.to_vec());
                format!("key: {}", text.trim())
            }
            Some(VoiceAction::NextTab) => self.voice_switch_tab(1.0),
            Some(VoiceAction::PrevTab) => self.voice_switch_tab(-1.0),
            Some(VoiceAction::Navigate(query)) => self.voice_navigate(&query),
            None => {
                let mut text = Self::sanitize_paste(text).replace("\r\n", "\r").replace('\n', "\r");
                if text.is_empty() {
                    return "nothing to type".into();
                }
                if self.config.voice.trailing_space {
                    text.push(' ');
                }
                self.voice_send(text.into_bytes());
                "typed".into()
            }
        }
    }

    /// Write bytes to the pinned terminal; fall back to the owner window's
    /// focused terminal, then the current window's.
    fn voice_send(&mut self, bytes: Vec<u8>) {
        let pinned = self.voice_target.and_then(|t| self.find_term(t)).map(|(wi, ti)| &self.wins[wi].terms[ti]);
        let tab = pinned.or_else(|| {
            let wi = self.voice_owner.and_then(|id| self.wins.iter().position(|w| w.window.id() == id)).unwrap_or(self.cur);
            self.wins[wi].active_term()
        });
        if let Some(tab) = tab {
            tab.write(bytes);
            tab.scroll(Scroll::Bottom);
        }
    }

    /// Index of the window that owns the session (the current one if none).
    fn voice_owner_index(&self) -> usize {
        self.voice_owner.and_then(|id| self.wins.iter().position(|w| w.window.id() == id)).unwrap_or(self.cur)
    }

    /// "Next tab" / "previous tab" in the owning window; dictation follows.
    fn voice_switch_tab(&mut self, dir: f32) -> String {
        let wi = self.voice_owner_index();
        self.cur = wi;
        let n = self.wins[wi].canvases.len();
        if n < 2 {
            return "only one tab".into();
        }
        let cur = self.wins[wi].active;
        let next = if dir > 0.0 { (cur + 1) % n } else { (cur + n - 1) % n };
        self.switch_tab_dir(next, dir);
        self.voice_target = self.wins[wi].active_term().map(|t| t.id);
        let title = self.wins[wi].tab_title(next);
        self.set_status(format!("Voice: {title}"));
        format!("switched to tab {title:?}")
    }

    /// "Switch to <query>": fuzzy-match card names and titles and tab names
    /// across all windows, then focus the best one and pin dictation to it.
    /// Names the user gave (a renamed card or canvas) outrank live titles.
    fn voice_navigate(&mut self, query: &str) -> String {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let mut buf = Vec::new();
        // (score, window, canvas, item, label)
        let mut best: Option<(u32, usize, usize, Option<ItemId>, String)> = None;
        for (wi, w) in self.wins.iter().enumerate() {
            for (ci, c) in w.canvases.iter().enumerate() {
                let mut cands: Vec<(String, bool, Option<ItemId>)> = Vec::new();
                if c.is_single() {
                    // A plain tab: its terminal is the only target.
                } else if !c.name.is_empty() {
                    cands.push((c.name.clone(), true, None));
                } else {
                    cands.push((w.tab_title(ci), false, None));
                }
                for it in &c.items {
                    let ItemKind::Terminal(t) = &it.kind else { continue };
                    if it.mirror {
                        continue;
                    }
                    let Some(term) = w.term(*t) else { continue };
                    if let Some(n) = term.custom_title.as_ref().or(it.name.as_ref()) {
                        cands.push((n.clone(), true, Some(it.id)));
                    }
                    cands.push((term.display_title().to_string(), false, Some(it.id)));
                }
                for (label, named, item) in cands {
                    buf.clear();
                    let hay = Utf32Str::new(&label, &mut buf);
                    if let Some(s) = pattern.score(hay, &mut matcher) {
                        let s = s + crate::voice::rank_bonus(&label, query) + if named { 1000 } else { 0 };
                        if best.as_ref().map(|b| s > b.0).unwrap_or(true) {
                            best = Some((s, wi, ci, item, label));
                        }
                    }
                }
            }
        }
        let Some((_, wi, ci, item, label)) = best else {
            self.set_status(format!("Voice: nothing called \"{query}\""));
            return format!("no card or tab matches {query:?}");
        };
        // Own the destination window before focusing it, so the old
        // window's focus-loss does not end the session.
        self.voice_owner = Some(self.wins[wi].window.id());
        self.cur = wi;
        self.switch_tab(ci);
        if let Some(id) = item {
            self.focus_item(id);
            if let Some(t) = self.tab_of_item_pub(id) {
                self.focus_terminal(t);
            }
        }
        self.wins[wi].window.focus_window();
        self.voice_target = self.wins[wi].active_term().map(|t| t.id);
        self.set_status(format!("Voice: switched to {label}"));
        self.request_redraw_all();
        format!("switched to {label:?}")
    }

    /// Text for the tab bar's status area while voice input is active in
    /// the window being drawn, and whether to draw it in the accent color.
    pub(super) fn voice_status(&self) -> Option<(String, bool)> {
        let v = self.voice.as_ref()?;
        if self.voice_owner != Some(self.win().window.id()) {
            return None;
        }
        Some(match v.state {
            VoiceState::Loading if v.start_when_ready => ("◌ loading the voice model…".into(), false),
            VoiceState::Listening => {
                let filled = (v.level * 8.0).round() as usize;
                let meter: String = (0..8).map(|i| if i < filled { '▮' } else { '▯' }).collect();
                (format!("● listening {meter}   Ctrl+Shift+M stops"), true)
            }
            VoiceState::Transcribing => ("● transcribing…".into(), true),
            VoiceState::Loading | VoiceState::Idle | VoiceState::Error => return None,
        })
    }
}
