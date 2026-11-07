//! Slint's `FocusScope` key event as our [`KeyEvent`].
//!
//! Slint reports a key as text only: control characters for Backspace, Tab,
//! Return, Escape and Delete, and private-use characters for the rest.

use crate::key::{Key, KeyAction, KeyEvent, Modifiers};

/// What Slint reported, as the key the encoder expects.
pub(crate) fn from_slint(event: &slint::language::KeyEvent) -> KeyEvent {
    let mods = modifiers(event);
    let text = event.text.as_str();
    let mut key_event = KeyEvent {
        action: if event.repeat {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        },
        key: map_key(text),
        mods,
        consumed_mods: consumed_modifiers(event, mods),
        text: (!text.is_empty()).then(|| text.to_string()),
        composing: false,
    };

    // Slint delivers a surrogate control character for a bare modifier
    // press (Shift → `\u{0010}`); strip it so nothing reaches the pty.
    if !mods.is_empty()
        // Tab/Enter/Backspace/Esc are not suppressed.
        && matches!(text.as_bytes(), [0x10..0x20])
    {
        key_event.key = None;
        key_event.text = None;
    }
    key_event
}

fn modifiers(event: &slint::language::KeyEvent) -> Modifiers {
    let m = event.modifiers;
    [
        (m.shift, Modifiers::SHIFT),
        (m.control, Modifiers::CTRL),
        (m.alt, Modifiers::ALT),
        (m.meta, Modifiers::SUPER),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .fold(Modifiers::empty(), |all, (_, flag)| all | flag)
}

fn consumed_modifiers(event: &slint::language::KeyEvent, mods: Modifiers) -> Modifiers {
    // AltGr arrives as ctrl+alt on X11/Wayland. If the platform produced one
    // printable character that isn't an ASCII letter, those modifiers were
    // spent on it and must not also turn into ESC/control bytes.
    Some(mods)
        .filter(|mods| mods.contains(Modifiers::CTRL | Modifiers::ALT))
        .filter(|_| {
            let mut chars = event.text.chars();
            matches!((chars.next(), chars.next()), (Some(ch), None) if !ch.is_ascii_alphabetic())
        })
        .unwrap_or_default()
}

fn map_key(text: &str) -> Option<Key> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }

    // Slint reports named keys as text: control characters, and private-use
    // characters from its `key_codes` (internal/common/key_codes.rs).
    let key = match ch {
        '\u{8}' => Key::Backspace,
        '\t' => Key::Tab,
        '\n' | '\r' => Key::Enter,
        '\u{1b}' => Key::Escape,
        '\u{7f}' => Key::Delete,
        '\u{F700}' => Key::Up,
        '\u{F701}' => Key::Down,
        '\u{F702}' => Key::Left,
        '\u{F703}' => Key::Right,
        '\u{F704}'..='\u{F71B}' => Key::F((ch as u32 - 0xF704 + 1) as u8),
        '\u{F727}' => Key::Insert,
        '\u{F729}' => Key::Home,
        '\u{F72B}' => Key::End,
        '\u{F72C}' => Key::PageUp,
        '\u{F72D}' => Key::PageDown,
        _ => Key::Char(ch),
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slint_event(text: &str, ctrl: bool, alt: bool, shift: bool) -> slint::language::KeyEvent {
        let mut event = slint::language::KeyEvent::default();
        event.text = text.into();
        event.modifiers.control = ctrl;
        event.modifiers.alt = alt;
        event.modifiers.shift = shift;
        event
    }

    #[test]
    fn named_keys_come_from_private_use_characters() {
        assert_eq!(map_key("\u{F700}"), Some(Key::Up));
        assert_eq!(map_key("\u{F704}"), Some(Key::F(1)));
        assert_eq!(map_key("\u{F71B}"), Some(Key::F(24)));
        assert_eq!(map_key("\u{8}"), Some(Key::Backspace));
        assert_eq!(map_key("a"), Some(Key::Char('a')));
        assert_eq!(map_key("ab"), None);
        assert_eq!(map_key(""), None);
    }

    #[test]
    fn altgr_character_consumes_its_modifiers() {
        let both = Modifiers::CTRL | Modifiers::ALT;
        assert_eq!(
            from_slint(&slint_event("@", true, true, false)).consumed_mods,
            both
        );
        // A real Ctrl+Alt shortcut keeps its modifiers.
        assert!(
            from_slint(&slint_event("c", true, true, false))
                .consumed_mods
                .is_empty()
        );
        // Alt alone with a symbol or digit is still a meta prefix.
        assert!(
            from_slint(&slint_event("1", false, true, false))
                .consumed_mods
                .is_empty()
        );
    }

    #[test]
    fn bare_modifier_press_sends_nothing() {
        let event = from_slint(&slint_event("\u{10}", false, false, true));
        assert_eq!((event.key, event.text), (None, None));
        // Without a modifier held, Tab-like control characters stay.
        assert_eq!(
            from_slint(&slint_event("\t", false, false, false)).key,
            Some(Key::Tab)
        );
    }

    #[test]
    fn repeat_is_kept() {
        let mut event = slint_event("a", false, false, false);
        event.repeat = true;
        assert_eq!(from_slint(&event).action, KeyAction::Repeat);
    }
}
