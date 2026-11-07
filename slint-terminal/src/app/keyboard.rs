//! Keyboard input: key events, shortcuts, paste and the pty writer.

use std::borrow::Cow;

use rio_vt::ansi::KeyboardModes;
use rio_vt::crosswords::Mode;
use rio_vt::crosswords::grid::Scroll;
use rio_vt::event::Msg;
use slint::private_unstable_api::re_exports::EventResult;

use crate::clipboard;
use crate::key::{self, EncodeContext, Key, KeyAction, KeyEvent, KittyFlags, Modifiers};
use crate::slint_key;

use super::*;

impl TerminalApp {
    /// A key from Slint's `FocusScope`.
    pub fn handle_key(&mut self, event: &slint::language::KeyEvent) -> EventResult {
        // Slint never reports releases, so there is nothing to pair with.
        self.dispatch_key(slint_key::from_slint(event), false);
        EventResult::Accept
    }

    /// A key from a source Slint cannot see (e.g. read from a VT), already translated.
    pub fn handle_key_event(&mut self, event: KeyEvent) {
        self.dispatch_key(event, true);
    }

    /// Run a shortcut or encode the key for the pty. `track_releases` remembers
    /// swallowed shortcut keys so their release is swallowed too.
    fn dispatch_key(&mut self, event: KeyEvent, track_releases: bool) {
        self.note_input();
        let text = event.text.clone().unwrap_or_default();
        match event.action {
            KeyAction::Press | KeyAction::Repeat => {
                let repeat = event.action == KeyAction::Repeat;
                if self.handle_shortcut(event.mods, &text, event.key, repeat) {
                    if track_releases
                        && let Some(key) = event.key
                        && !self.shortcut_keys.contains(&key)
                    {
                        self.shortcut_keys.push(key);
                    }
                    return;
                }
            }
            // The release of a shortcut must not leak through (kitty release
            // reporting would otherwise see a key that never went down).
            KeyAction::Release => {
                if let Some(key) = event.key
                    && let Some(position) = self.shortcut_keys.iter().position(|&k| k == key)
                {
                    self.shortcut_keys.swap_remove(position);
                    return;
                }
            }
        }
        self.send_key(&event);
    }

    pub(super) fn send_key(&mut self, key_event: &KeyEvent) {
        let context = self.encode_context();
        if let Some(bytes) = key::encode(key_event, &context) {
            self.write_input(bytes);
        }
    }

    /// Host shortcuts (copy/paste, zoom, scrollback) that are not forwarded to
    /// the pty; true if `key` was one. `repeat`: an auto-repeating key scrolls
    /// and zooms again but must not paste again.
    pub(super) fn handle_shortcut(
        &mut self,
        mods: Modifiers,
        text: &str,
        key: Option<Key>,
        repeat: bool,
    ) -> bool {
        enum Action {
            Paste(clipboard::Kind),
            Copy,
            Zoom(bool),
        }
        let text = text.to_ascii_lowercase();
        let ctrl_shift = mods.contains(Modifiers::CTRL | Modifiers::SHIFT);
        let ctrl_only = mods.contains(Modifiers::CTRL) && !mods.contains(Modifiers::SHIFT);

        // Shift+PageUp/PageDown/Home/End scroll the scrollback, as on the
        // Linux console. On the alternate screen the program owns the keys.
        if mods == Modifiers::SHIFT {
            let scroll = match key {
                Some(Key::PageUp) => Some(Scroll::PageUp),
                Some(Key::PageDown) => Some(Scroll::PageDown),
                Some(Key::Home) => Some(Scroll::Top),
                Some(Key::End) => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                let mut term = self.term.lock();
                if !term.mode().contains(Mode::ALT_SCREEN) {
                    term.scroll_display(scroll);
                    return true;
                }
            }
        }

        // Ctrl+Shift+C/V, Ctrl+Insert and Shift+Insert (primary selection).
        // Copy swallows the key even without a selection rather than sending ^C.
        let action = match (key, text.as_str()) {
            (_, "v") if ctrl_shift => Action::Paste(clipboard::Kind::Clipboard),
            (_, "c") if ctrl_shift => Action::Copy,
            (Some(Key::Insert), _) if mods == Modifiers::CTRL => Action::Copy,
            (Some(Key::Insert), _) if mods == Modifiers::SHIFT => {
                Action::Paste(clipboard::Kind::Primary)
            }
            (_, "+" | "=") if ctrl_only => Action::Zoom(true),
            (_, "-") if ctrl_only => Action::Zoom(false),
            _ => return false,
        };
        match action {
            Action::Paste(kind) if !repeat => self.paste(kind),
            Action::Copy if !repeat => self.copy_selection(),
            // Ctrl+ and Ctrl- step to the adjacent zoom level.
            Action::Zoom(larger) => {
                self.set_font_px(self.renderer.zoom().step(self.font.px, larger));
                self.render();
            }
            _ => {}
        }
        true
    }

    /// Modifiers the host knows when Slint cannot see the keyboard.
    pub fn set_external_modifiers(&mut self, mods: Option<Modifiers>) {
        self.external_modifiers = mods;
    }

    pub(super) fn send_pty(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.pty.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    pub(super) fn encode_context(&self) -> EncodeContext {
        let term = self.term.lock();
        let modes = term.keyboard_mode();
        let kitty = [
            (
                KeyboardModes::DISAMBIGUATE_ESC_CODES,
                KittyFlags::DISAMBIGUATE,
            ),
            (
                KeyboardModes::REPORT_EVENT_TYPES,
                KittyFlags::REPORT_EVENT_TYPES,
            ),
            (
                KeyboardModes::REPORT_ALL_KEYS_AS_ESC,
                KittyFlags::REPORT_ALL_AS_ESC,
            ),
        ]
        .into_iter()
        .filter(|(mode, _)| modes.contains(*mode))
        .fold(KittyFlags::empty(), |all, (_, flag)| all | flag);

        EncodeContext {
            app_cursor: term.mode().contains(Mode::APP_CURSOR),
            kitty,
            modify_other_keys: term.modify_other_keys(),
            alt_is_meta: true,
        }
    }

    pub(super) fn paste(&mut self, kind: clipboard::Kind) {
        let Some(text) = self.clipboard.get(kind) else {
            return;
        };

        let bracketed = self.term.lock().mode().contains(Mode::BRACKETED_PASTE);
        let mut paste = String::with_capacity(text.len() + 12);
        if bracketed {
            paste.push_str("\x1b[200~");
        }
        for ch in text.chars() {
            match ch {
                // Pasting should not be able to inject control sequences.
                '\x1b' | '\x03' | '\u{9b}' | '\r' => {}
                '\n' => paste.push('\r'),
                _ => paste.push(ch),
            }
        }
        if bracketed {
            paste.push_str("\x1b[201~");
        }
        self.write_input(paste.into_bytes());
    }

    pub(super) fn write_input(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Bottom);
        term.selection = None;
        let _ = self.pty.send(Msg::Input(Cow::Owned(bytes)));
    }
}
