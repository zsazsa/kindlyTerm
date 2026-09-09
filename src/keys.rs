//! Translate winit key events into the byte sequences a terminal expects.

use alacritty_terminal::term::TermMode;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn mod_param(m: ModifiersState) -> u8 {
    let mut p = 1;
    if m.shift_key() {
        p += 1;
    }
    if m.alt_key() {
        p += 2;
    }
    if m.control_key() {
        p += 4;
    }
    p
}

/// CSI sequence for a cursor-style key: `ESC [ A` or `ESC O A` (app cursor),
/// or `ESC [ 1 ; mod A` with modifiers.
fn cursor_key(m: ModifiersState, mode: TermMode, ch: char) -> Vec<u8> {
    let p = mod_param(m);
    if p == 1 {
        if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{ch}").into_bytes()
        } else {
            format!("\x1b[{ch}").into_bytes()
        }
    } else {
        format!("\x1b[1;{p}{ch}").into_bytes()
    }
}

/// `ESC [ n ~` or `ESC [ n ; mod ~`.
fn tilde_key(m: ModifiersState, n: u8) -> Vec<u8> {
    let p = mod_param(m);
    if p == 1 { format!("\x1b[{n}~").into_bytes() } else { format!("\x1b[{n};{p}~").into_bytes() }
}

/// `ESC O P` style for F1-F4 without modifiers, `ESC [ 1 ; mod P` with.
fn ss3_key(m: ModifiersState, ch: char) -> Vec<u8> {
    let p = mod_param(m);
    if p == 1 { format!("\x1bO{ch}").into_bytes() } else { format!("\x1b[1;{p}{ch}").into_bytes() }
}

/// Returns the bytes to write to the PTY for this key event, or None if the
/// key produces nothing (e.g. a bare modifier). Applications that enabled
/// the kitty keyboard protocol get its encoding; everyone else the legacy
/// xterm one. Releases only produce bytes under the kitty protocol with
/// event reporting on.
pub fn encode(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        return encode_kitty(event, mods, mode);
    }
    if event.state != ElementState::Pressed {
        return None;
    }
    encode_legacy(event, mods, mode)
}

/// How a key is spelled in the kitty protocol.
enum KittyKey {
    /// `CSI code ; mods u`
    U(u32),
    /// `CSI code ; mods ~`
    Tilde(u32),
    /// `CSI 1 ; mods X` (arrows, Home/End, F1/F2/F4)
    Letter(char),
}

/// The parts of a key event the kitty encoder needs (kept free of winit
/// types so it can be tested).
pub struct KeyParts<'a> {
    pub named: Option<NamedKey>,
    /// For character keys: the key as typed (with Shift), its unshifted
    /// base, and the produced text.
    pub shifted: Option<char>,
    pub base: Option<char>,
    pub text: Option<&'a str>,
    pub pressed: bool,
    pub repeat: bool,
    pub mods: ModifiersState,
}

impl<'a> KeyParts<'a> {
    fn from_event(event: &'a KeyEvent, mods: ModifiersState) -> Self {
        let (named, shifted) = match &event.logical_key {
            Key::Named(k) => (Some(*k), None),
            Key::Character(s) => (None, s.chars().next()),
            _ => (None, None),
        };
        let base = match event.key_without_modifiers() {
            Key::Character(b) => b.chars().next(),
            _ => None,
        };
        Self { named, shifted, base, text: event.text.as_deref(), pressed: event.state == ElementState::Pressed, repeat: event.repeat, mods }
    }
}

/// Kitty keyboard protocol (progressive enhancement flags are in `mode`).
/// See https://sw.kovidgoyal.net/kitty/keyboard-protocol/
fn encode_kitty(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    let parts = KeyParts::from_event(event, mods);
    encode_kitty_parts(&parts, mode, || {
        if event.state == ElementState::Pressed { encode_legacy(event, mods, mode) } else { None }
    })
}

/// `legacy` supplies the xterm spelling for keys the protocol leaves alone.
pub fn encode_kitty_parts(k: &KeyParts, mode: TermMode, legacy: impl Fn() -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mods = k.mods;
    let report_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let report_events = mode.contains(TermMode::REPORT_EVENT_TYPES);
    let alt_keys = mode.contains(TermMode::REPORT_ALTERNATE_KEYS);
    let with_text = mode.contains(TermMode::REPORT_ASSOCIATED_TEXT);
    let ev: u8 = match (k.pressed, k.repeat) {
        (false, _) => 3,
        (true, true) => 2,
        (true, false) => 1,
    };
    if ev == 3 && !report_events {
        return None;
    }
    let mut m: u32 = 0;
    if mods.shift_key() {
        m |= 1;
    }
    if mods.alt_key() {
        m |= 2;
    }
    if mods.control_key() {
        m |= 4;
    }
    if mods.super_key() {
        m |= 8;
    }
    let plain = m == 0;
    // Modifier field: "mods" or "mods:event", omitted entirely when both
    // are defaults and no text field follows.
    let mods_field = |need: bool| -> String {
        if ev != 1 { format!("{}:{ev}", m + 1) } else if m != 0 || need { format!("{}", m + 1) } else { String::new() }
    };
    let finish = |key: String, term: char, text: Option<String>| -> Vec<u8> {
        let mf = mods_field(text.is_some());
        let mut out = format!("\x1b[{key}");
        if !mf.is_empty() || text.is_some() {
            out.push(';');
            out.push_str(&mf);
        }
        if let Some(t) = text {
            out.push(';');
            out.push_str(&t);
        }
        out.push(term);
        out.into_bytes()
    };

    let named = |k: &NamedKey| -> Option<KittyKey> {
        Some(match k {
            NamedKey::Escape => KittyKey::U(27),
            NamedKey::Enter => KittyKey::U(13),
            NamedKey::Tab => KittyKey::U(9),
            NamedKey::Backspace => KittyKey::U(127),
            NamedKey::Insert => KittyKey::Tilde(2),
            NamedKey::Delete => KittyKey::Tilde(3),
            NamedKey::ArrowLeft => KittyKey::Letter('D'),
            NamedKey::ArrowRight => KittyKey::Letter('C'),
            NamedKey::ArrowUp => KittyKey::Letter('A'),
            NamedKey::ArrowDown => KittyKey::Letter('B'),
            NamedKey::PageUp => KittyKey::Tilde(5),
            NamedKey::PageDown => KittyKey::Tilde(6),
            NamedKey::Home => KittyKey::Letter('H'),
            NamedKey::End => KittyKey::Letter('F'),
            NamedKey::F1 => KittyKey::Letter('P'),
            NamedKey::F2 => KittyKey::Letter('Q'),
            NamedKey::F3 => KittyKey::Tilde(13),
            NamedKey::F4 => KittyKey::Letter('S'),
            NamedKey::F5 => KittyKey::Tilde(15),
            NamedKey::F6 => KittyKey::Tilde(17),
            NamedKey::F7 => KittyKey::Tilde(18),
            NamedKey::F8 => KittyKey::Tilde(19),
            NamedKey::F9 => KittyKey::Tilde(20),
            NamedKey::F10 => KittyKey::Tilde(21),
            NamedKey::F11 => KittyKey::Tilde(23),
            NamedKey::F12 => KittyKey::Tilde(24),
            NamedKey::CapsLock => KittyKey::U(57358),
            NamedKey::ScrollLock => KittyKey::U(57359),
            NamedKey::NumLock => KittyKey::U(57360),
            NamedKey::PrintScreen => KittyKey::U(57361),
            NamedKey::Pause => KittyKey::U(57362),
            NamedKey::ContextMenu => KittyKey::U(57363),
            NamedKey::Shift => KittyKey::U(57441),
            NamedKey::Control => KittyKey::U(57442),
            NamedKey::Alt => KittyKey::U(57443),
            NamedKey::Super => KittyKey::U(57444),
            NamedKey::Space => KittyKey::U(32),
            _ => return None,
        })
    };

    if let Some(nk) = k.named {
        if nk == NamedKey::Space && !report_all && plain && ev == 1 {
            return Some(b" ".to_vec());
        }
        let kk = named(&nk)?;
        let is_modifier = matches!(nk, NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Super);
        if is_modifier && !report_all {
            return None;
        }
        // Legacy spellings survive for unmodified presses unless the
        // application asked for every key as an escape code. Escape itself
        // is the one that always changes (that is the point).
        if !report_all && plain && ev == 1 && nk != NamedKey::Escape {
            return legacy();
        }
        return Some(match kk {
            KittyKey::U(c) => finish(c.to_string(), 'u', None),
            KittyKey::Tilde(c) => finish(c.to_string(), '~', None),
            KittyKey::Letter(l) => finish("1".into(), l, None),
        });
    }
    let shifted = k.shifted?;
    let base = k.base.unwrap_or(shifted);
    let base_lc = base.to_lowercase().next().unwrap_or(base);
    let text_only = !mods.control_key() && !mods.alt_key() && !mods.super_key();
    if text_only && !report_all && ev == 1 {
        // Plain typing (Shift included) stays text.
        let text = k.text.unwrap_or("");
        let text = if text.is_empty() { shifted.to_string() } else { text.to_string() };
        return Some(text.into_bytes());
    }
    let mut key = (base_lc as u32).to_string();
    if alt_keys && mods.shift_key() && shifted != base_lc {
        key = format!("{}:{}", base_lc as u32, shifted as u32);
    }
    let text = if with_text && ev != 3 && text_only {
        k.text.filter(|t| !t.is_empty()).map(|t| t.chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>().join(":"))
    } else {
        None
    };
    Some(finish(key, 'u', text))
}

/// xterm-style encoding used by everything that has not opted in to the
/// kitty protocol.
fn encode_legacy(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();

    let bytes: Vec<u8> = match &event.logical_key {
        Key::Named(named) => match named {
            NamedKey::Enter => {
                if alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }
            }
            NamedKey::Backspace => {
                let base: &[u8] = if ctrl { b"\x08" } else { b"\x7f" };
                if alt { [b"\x1b", base].concat() } else { base.to_vec() }
            }
            NamedKey::Tab => {
                if shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }
            }
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::Space => {
                if ctrl { vec![0] } else if alt { b"\x1b ".to_vec() } else { b" ".to_vec() }
            }
            NamedKey::ArrowUp => cursor_key(mods, mode, 'A'),
            NamedKey::ArrowDown => cursor_key(mods, mode, 'B'),
            NamedKey::ArrowRight => cursor_key(mods, mode, 'C'),
            NamedKey::ArrowLeft => cursor_key(mods, mode, 'D'),
            NamedKey::Home => cursor_key(mods, mode, 'H'),
            NamedKey::End => cursor_key(mods, mode, 'F'),
            NamedKey::Insert => tilde_key(mods, 2),
            NamedKey::Delete => tilde_key(mods, 3),
            NamedKey::PageUp => tilde_key(mods, 5),
            NamedKey::PageDown => tilde_key(mods, 6),
            NamedKey::F1 => ss3_key(mods, 'P'),
            NamedKey::F2 => ss3_key(mods, 'Q'),
            NamedKey::F3 => ss3_key(mods, 'R'),
            NamedKey::F4 => ss3_key(mods, 'S'),
            NamedKey::F5 => tilde_key(mods, 15),
            NamedKey::F6 => tilde_key(mods, 17),
            NamedKey::F7 => tilde_key(mods, 18),
            NamedKey::F8 => tilde_key(mods, 19),
            NamedKey::F9 => tilde_key(mods, 20),
            NamedKey::F10 => tilde_key(mods, 21),
            NamedKey::F11 => tilde_key(mods, 23),
            NamedKey::F12 => tilde_key(mods, 24),
            _ => return None,
        },
        Key::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if ctrl {
                // Control characters for the ASCII range.
                let byte = match c.to_ascii_lowercase() {
                    'a'..='z' => Some((c.to_ascii_uppercase() as u8) & 0x1f),
                    '[' | '3' => Some(0x1b),
                    '\\' | '4' => Some(0x1c),
                    ']' | '5' => Some(0x1d),
                    '^' | '6' => Some(0x1e),
                    '_' | '/' | '7' | '-' => Some(0x1f),
                    '2' | '@' => Some(0x00),
                    '8' | '?' => Some(0x7f),
                    _ => None,
                };
                {
                    let b = byte?;
                    if alt { vec![0x1b, b] } else { vec![b] }
                }
            } else {
                let text = event.text.as_deref().unwrap_or(s.as_str());
                if alt { [b"\x1b", text.as_bytes()].concat() } else { text.as_bytes().to_vec() }
            }
        }
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(named: Option<NamedKey>, ch: Option<(char, char, &'static str)>, pressed: bool, repeat: bool, mods: ModifiersState) -> KeyParts<'static> {
        KeyParts { named, shifted: ch.map(|c| c.0), base: ch.map(|c| c.1), text: ch.map(|c| c.2), pressed, repeat, mods }
    }
    fn enc(k: &KeyParts, flags: TermMode) -> String {
        String::from_utf8(encode_kitty_parts(k, flags, || Some(b"LEGACY".to_vec())).unwrap_or_default()).unwrap()
    }
    const D: TermMode = TermMode::DISAMBIGUATE_ESC_CODES;
    const E: TermMode = TermMode::REPORT_EVENT_TYPES;
    const A: TermMode = TermMode::REPORT_ALTERNATE_KEYS;
    const ALL: TermMode = TermMode::REPORT_ALL_KEYS_AS_ESC;
    const T: TermMode = TermMode::REPORT_ASSOCIATED_TEXT;

    #[test]
    fn disambiguate() {
        let none = ModifiersState::empty();
        assert_eq!(enc(&parts(Some(NamedKey::Escape), None, true, false, none), D), "\x1b[27u");
        assert_eq!(enc(&parts(Some(NamedKey::Enter), None, true, false, none), D), "LEGACY");
        assert_eq!(enc(&parts(Some(NamedKey::Enter), None, true, false, ModifiersState::CONTROL), D), "\x1b[13;5u");
        assert_eq!(enc(&parts(Some(NamedKey::Backspace), None, true, false, ModifiersState::ALT), D), "\x1b[127;3u");
        assert_eq!(enc(&parts(None, Some(('a', 'a', "a")), true, false, none), D), "a");
        assert_eq!(enc(&parts(None, Some(('A', 'a', "A")), true, false, ModifiersState::SHIFT), D), "A");
        assert_eq!(enc(&parts(None, Some(('a', 'a', "")), true, false, ModifiersState::CONTROL), D), "\x1b[97;5u");
        assert_eq!(enc(&parts(Some(NamedKey::ArrowUp), None, true, false, ModifiersState::CONTROL), D), "\x1b[1;5A");
        assert_eq!(enc(&parts(Some(NamedKey::F3), None, true, false, ModifiersState::SHIFT), D), "\x1b[13;2~");
        // Releases are silent without event reporting.
        assert_eq!(enc(&parts(None, Some(('a', 'a', "")), false, false, none), D), "");
    }

    #[test]
    fn events_and_alternates() {
        let none = ModifiersState::empty();
        assert_eq!(enc(&parts(None, Some(('a', 'a', "")), false, false, none), D | E), "\x1b[97;1:3u");
        assert_eq!(enc(&parts(None, Some(('a', 'a', "")), true, true, ModifiersState::CONTROL), D | E), "\x1b[97;5:2u");
        assert_eq!(enc(&parts(None, Some(('A', 'a', "A")), true, false, ModifiersState::SHIFT), ALL | A), "\x1b[97:65;2u");
        assert_eq!(enc(&parts(None, Some(('a', 'a', "a")), true, false, none), ALL), "\x1b[97u");
        assert_eq!(enc(&parts(Some(NamedKey::Enter), None, true, false, none), ALL), "\x1b[13u");
        assert_eq!(enc(&parts(None, Some(('a', 'a', "a")), true, false, none), ALL | T), "\x1b[97;1;97u");
        assert_eq!(enc(&parts(Some(NamedKey::Shift), None, true, false, ModifiersState::SHIFT), ALL), "\x1b[57441;2u");
    }
}
