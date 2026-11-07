//! Keycodes to key events, following the kernel's keyboard driver.

use slint_terminal::key::{Key, KeyAction, KeyEvent, Modifiers};

use super::keymap::*;

/// `KDGKBLED` flag bits.
const LED_NUM: u8 = 0x02;
const LED_CAP: u8 = 0x04;

// `shift_state` bits: `1 << KG_*`, which is also the keymap table index
// (`1 << 1` is AltGr: it only selects a table, it is not a modifier).
const SHIFT: u16 = 1 << 0;
const CTRL: u16 = 1 << 2;
const ALT: u16 = 1 << 3;
const SHIFTL: u16 = 1 << 4;
const SHIFTR: u16 = 1 << 5;
const CTRLL: u16 = 1 << 6;
const CTRLR: u16 = 1 << 7;
const ANY_SHIFT: u16 = SHIFT | SHIFTL | SHIFTR;
const ANY_CTRL: u16 = CTRL | CTRLL | CTRLR;
const KG_CAPSSHIFT: u8 = 8;
const NR_SHIFT: usize = 9;

/// What a held key was when it went down, so its release matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Held {
    Shift(u8),
    Key(Key),
    /// Produced nothing (dead key, console switch, unmapped key).
    Silent,
}

#[derive(PartialEq, Eq, Debug)]
pub(super) enum Output {
    Key(KeyEvent),
    /// A console-switch key of the keymap was pressed (1-based VT).
    Switch(u16),
}

/// Keycodes to key events, following the kernel's keyboard driver.
pub(super) struct Translator {
    keymap: Keymap,
    down: [bool; NR_KEYS],
    held: [Option<Held>; NR_KEYS],
    shift_down: [u8; NR_SHIFT],
    shift_state: u16,
    /// A dead key waits for the next character.
    pending_dead: Option<char>,
}

impl Translator {
    pub(super) fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            down: [false; NR_KEYS],
            held: [None; NR_KEYS],
            shift_down: [0; NR_SHIFT],
            shift_state: 0,
            pending_dead: None,
        }
    }

    /// Forget held keys, modifiers and a pending dead key (VT switch,
    /// keyboard mode changed behind our back).
    pub(super) fn reset(&mut self) {
        self.down = [false; NR_KEYS];
        self.held = [None; NR_KEYS];
        self.shift_down = [0; NR_SHIFT];
        self.shift_state = 0;
        self.pending_dead = None;
    }

    /// One keycode edge. `leds`: the `KDGKBLED` flags (CapsLock, NumLock).
    pub(super) fn key(&mut self, keycode: u16, down: bool, leds: u8) -> Option<Output> {
        // The keymap has 256 keys; media keys beyond that have no entry.
        let keycode = usize::from(keycode);
        if keycode == 0 || keycode >= NR_KEYS {
            return None;
        }
        if !down {
            if !std::mem::replace(&mut self.down[keycode], false) {
                return None;
            }
            return match self.held[keycode].take()? {
                Held::Shift(value) => {
                    self.shift(value, false);
                    None
                }
                Held::Key(key) => Some(Output::Key(self.event(KeyAction::Release, key, None))),
                Held::Silent => None,
            };
        }

        let repeat = std::mem::replace(&mut self.down[keycode], true);
        // A modifier is one in the plain table whatever else is held; the
        // kernel looks in the current table, which only matters for keymaps
        // that leave a modifier out of a table.
        let sym = match classify(self.keymap.sym(0, keycode)) {
            Sym::Shift(value) => K(KT_SHIFT, u16::from(value)),
            _ => self.lookup(keycode),
        };
        match classify(sym) {
            Sym::Shift(value) => {
                if !repeat {
                    self.shift(value, true);
                    self.held[keycode] = Some(Held::Shift(value));
                }
                None
            }
            Sym::Cons(console) => {
                self.held[keycode] = Some(Held::Silent);
                (!repeat).then_some(Output::Switch(u16::from(console)))
            }
            _ => {
                if repeat && self.held[keycode] == Some(Held::Silent) {
                    return None;
                }
                let (held, output) = self.press(keycode, repeat, leds);
                self.held[keycode] = Some(held);
                output
            }
        }
    }

    /// The entry the kernel would use for this key in the current state.
    fn lookup(&self, keycode: usize) -> u16 {
        let sym = self.keymap.sym(self.shift_state, keycode);
        if classify(sym) == Sym::Hole {
            self.keymap.sym(0, keycode)
        } else {
            sym
        }
    }

    fn shift(&mut self, value: u8, down: bool) {
        // CapsShift acts as Shift (the kernel also clears CapsLock).
        let value = if value == KG_CAPSSHIFT { 0 } else { value };
        let Some(count) = self.shift_down.get_mut(usize::from(value)) else {
            return;
        };
        *count = if down {
            count.saturating_add(1)
        } else {
            count.saturating_sub(1)
        };
        let bit = 1u16 << value;
        if *count > 0 {
            self.shift_state |= bit;
        } else {
            self.shift_state &= !bit;
        }
    }

    pub(super) fn mods(&self) -> Modifiers {
        let mut mods = Modifiers::empty();
        if self.shift_state & ANY_SHIFT != 0 {
            mods |= Modifiers::SHIFT;
        }
        if self.shift_state & ANY_CTRL != 0 {
            mods |= Modifiers::CTRL;
        }
        if self.shift_state & ALT != 0 {
            mods |= Modifiers::ALT;
        }
        mods
    }

    fn event(&self, action: KeyAction, key: Key, text: Option<String>) -> KeyEvent {
        KeyEvent {
            action,
            key: Some(key),
            mods: self.mods(),
            consumed_mods: Modifiers::empty(),
            text,
            composing: false,
        }
    }

    fn press(&mut self, keycode: usize, repeat: bool, leds: u8) -> (Held, Option<Output>) {
        let action = if repeat {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        let numlock = leds & LED_NUM != 0;

        // Keys that are not text are identified by the unmodified table, so
        // `shift+F1` is F1 with shift rather than the keymap's F13.
        let named: Option<(Option<Key>, Option<char>)> = match classify(self.keymap.sym(0, keycode))
        {
            Sym::Fn(value) => Some((fn_key(value), None)),
            Sym::Cur(value) => Some((cursor_key(value), None)),
            Sym::Csi(value) => Some((csi_key(value), None)),
            Sym::Enter => Some((Some(Key::Enter), None)),
            // Keypad symbols (`+`, digits with NumLock) also carry text.
            Sym::Pad(value) => Some(match pad_key(value, numlock) {
                Some((key, text)) => (Some(key), text),
                None => (None, None),
            }),
            _ => None,
        };
        if let Some((key, text)) = named {
            let Some(key) = key else {
                return (Held::Silent, None);
            };
            let event = self.event(action, key, text.map(String::from));
            return (Held::Key(key), Some(Output::Key(event)));
        }

        // Text: the table for shift/AltGr only. Control and Alt reach the
        // encoder as modifiers, which turns them into control bytes and ESC.
        let state = self.shift_state & !(ANY_CTRL | ALT);
        let table = self.text_table(state);
        let mut sym = classify(self.keymap.sym(table, keycode));
        if let Sym::Text { letter: true, .. } = sym
            && leds & LED_CAP != 0
        {
            sym = classify(self.keymap.sym(self.text_table(state ^ SHIFT), keycode));
        }

        match sym {
            Sym::Dead(value) => {
                if !repeat {
                    self.pending_dead = DEAD_CHARS.get(usize::from(value)).copied();
                }
                (Held::Silent, None)
            }
            Sym::Text { ch, .. } => {
                let mut text = String::new();
                match self.pending_dead.take() {
                    Some(accent) if !repeat => self.compose(accent, ch, &mut text),
                    _ => text.push(ch),
                }
                let key = self.base_key(keycode, state, ch);
                // Special keys travel as keys, not as their control byte.
                let text = (!matches!(key, Key::Enter | Key::Tab | Key::Backspace | Key::Escape))
                    .then_some(text);
                let event = self.event(action, key, text);
                (Held::Key(key), Some(Output::Key(event)))
            }
            _ => (Held::Silent, None),
        }
    }

    /// The table for `state`, falling back to plain Shift when the keymap has
    /// no table for the exact left/right shift combination.
    fn text_table(&self, state: u16) -> u16 {
        if self.keymap.has(state) {
            return state;
        }
        let collapsed = state & !ANY_SHIFT | if state & ANY_SHIFT != 0 { SHIFT } else { 0 };
        if self.keymap.has(collapsed) {
            collapsed
        } else {
            state
        }
    }

    /// The key's identity: its character without shift (`shift+a` is `a`).
    fn base_key(&self, keycode: usize, state: u16, shifted: char) -> Key {
        let table = self.text_table(state & !ANY_SHIFT);
        let ch = match classify(self.keymap.sym(table, keycode)) {
            Sym::Text { ch, .. } => ch,
            _ => shifted,
        };
        match ch {
            '\r' | '\n' => Key::Enter,
            '\t' => Key::Tab,
            // The Backspace key is `Delete` (0x7f) in console keymaps; the
            // Delete key is a function key.
            '\u{7f}' | '\u{8}' => Key::Backspace,
            '\u{1b}' => Key::Escape,
            ch => Key::Char(ch),
        }
    }

    /// The kernel's dead-key rule (`handle_diacr`).
    fn compose(&self, accent: char, ch: char, out: &mut String) {
        if ch == ' ' || ch == accent {
            out.push(accent);
        } else if let Some(composed) = self.keymap.compose(accent, ch) {
            out.push(composed);
        } else {
            out.push(accent);
            out.push(ch);
        }
    }
}

fn fn_key(value: u8) -> Option<Key> {
    Some(match value {
        0..=19 => Key::F(value + 1),
        20 => Key::Home,
        21 => Key::Insert,
        22 => Key::Delete,
        23 => Key::End,
        24 => Key::PageUp,
        25 => Key::PageDown,
        30..=33 => Key::F(value - 30 + 21),
        _ => return None,
    })
}

fn cursor_key(value: u8) -> Option<Key> {
    Some(match value {
        0 => Key::Down,
        1 => Key::Left,
        2 => Key::Right,
        3 => Key::Up,
        _ => return None,
    })
}

/// `KT_CSI` values are the `CSI n ~` parameter.
fn csi_key(value: u8) -> Option<Key> {
    Some(match value {
        1 => Key::Home,
        2 => Key::Insert,
        3 => Key::Delete,
        4 => Key::End,
        5 => Key::PageUp,
        6 => Key::PageDown,
        11..=15 => Key::F(value - 10),
        17..=21 => Key::F(value - 11),
        23..=26 => Key::F(value - 12),
        28 | 29 => Key::F(value - 13),
        31..=34 => Key::F(value - 15),
        _ => return None,
    })
}

/// Keypad keys as the kernel's `k_pad` handles them: digits and `.` follow
/// NumLock, the operators and Enter do not. Returns the key and its text.
fn pad_key(value: u8, numlock: bool) -> Option<(Key, Option<char>)> {
    const PAD_CHARS: &[u8] = b"0123456789+-*/\r,.?()#";
    let ch = char::from(*PAD_CHARS.get(usize::from(value))?);
    let nav = |key| Some((key, None));
    if !numlock {
        match value {
            15 | 16 => return nav(Key::Delete),
            0 => return nav(Key::Insert),
            1 => return nav(Key::End),
            2 => return nav(Key::Down),
            3 => return nav(Key::PageDown),
            4 => return nav(Key::Left),
            5 => return None,
            6 => return nav(Key::Right),
            7 => return nav(Key::Home),
            8 => return nav(Key::Up),
            9 => return nav(Key::PageUp),
            _ => {}
        }
    }
    if ch == '\r' {
        return nav(Key::Enter);
    }
    Some((Key::Char(ch), Some(ch)))
}
