//! Translate winit key events into the byte sequences a terminal expects.

use alacritty_terminal::term::TermMode;
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

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

/// Returns the bytes to write to the PTY for this key press, or None if the
/// key produces nothing (e.g. a bare modifier).
pub fn encode(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
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
