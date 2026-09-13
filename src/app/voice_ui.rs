//! Voice input glue: the Ctrl+Shift+M key, worker messages, typing the text.
//!
//! A voice session belongs to the window it was started in. Moving between
//! cards or canvases inside that window keeps it going; switching to another
//! window or application, or closing the window, stops it.

use super::*;
use crate::voice::{Voice, VoiceMsg, VoiceState};

impl App {
    /// The voice key went down. Tap: toggle listening. Hold: push-to-talk
    /// (the release in `voice_release` commits). A press while listening
    /// stops and commits.
    pub(super) fn voice_press(&mut self) {
        self.chord_armed = None;
        self.voice_key_down = Some(Instant::now());
        let here = self.win().window.id();
        let needs_engine = self.voice.as_ref().map(|v| v.state == VoiceState::Error).unwrap_or(true);
        if needs_engine {
            let proxy = self.proxy.clone();
            let mut v = Voice::spawn(self.config.voice.clone(), move || {
                let _ = proxy.send_event(UserEvent { tab: crate::terminal::SYS_VOICE, event: alacritty_terminal::event::Event::Wakeup });
            });
            v.start_when_ready = true;
            self.voice = Some(v);
            self.voice_owner = Some(here);
            self.set_status("Loading the voice model…".into());
            return;
        }
        let v = self.voice.as_mut().expect("voice");
        match v.state {
            VoiceState::Loading => {
                // A second tap while the model loads cancels the first.
                v.start_when_ready = !v.start_when_ready;
                let armed = v.start_when_ready;
                self.voice_owner = armed.then_some(here);
                if !armed {
                    self.voice_key_down = None;
                }
            }
            VoiceState::Idle => {
                v.start();
                self.voice_owner = Some(here);
            }
            VoiceState::Listening | VoiceState::Transcribing => {
                v.stop(true);
                // This press ended the session; its release must not act.
                self.voice_key_down = None;
            }
            VoiceState::Error => {}
        }
        self.request_redraw();
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
            self.voice_type(&t);
        }
        if let Some(e) = error {
            self.set_status(e);
        }
        self.request_redraw_all();
    }

    /// Type recognized text into the owning window's focused terminal as
    /// keystrokes (no bracketed paste, so the program sees ordinary typing).
    fn voice_type(&mut self, text: &str) {
        // An utterance that is just a command word ("enter") presses the key.
        let bytes = match crate::voice::command_bytes(text, &self.config.voice.commands) {
            Some(key) => key.to_vec(),
            None => {
                let mut text = Self::sanitize_paste(text).replace("\r\n", "\r").replace('\n', "\r");
                if text.is_empty() {
                    return;
                }
                if self.config.voice.trailing_space {
                    text.push(' ');
                }
                text.into_bytes()
            }
        };
        let Some(owner) = self.voice_owner else { return };
        let Some(w) = self.wins.iter().find(|w| w.window.id() == owner) else { return };
        if let Some(tab) = w.active_term() {
            tab.write(bytes);
            tab.scroll(Scroll::Bottom);
        }
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
