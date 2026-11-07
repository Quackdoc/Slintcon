//! Key events and their encoding to pty bytes, adapted from Rio's
//! `librio/src/key.rs` (MIT).
//!
//! Covers the legacy encodings, `modifyOtherKeys`, and the kitty keyboard
//! protocol's disambiguation, event types and report-all-as-escape flags;
//! alternate keys and associated text degrade to the disambiguating form.

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Modifiers: u8 {
        const SHIFT = 1 << 0;
        const CTRL  = 1 << 1;
        const ALT   = 1 << 2;
        const SUPER = 1 << 3;
    }
}

bitflags::bitflags! {
    /// Which of the kitty keyboard flags the terminal currently has set.
    /// The subset of `rio_vt::ansi::KeyboardModes` this encoder implements. The
    /// bit values differ from rio's, so convert flag by flag.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct KittyFlags: u8 {
        const DISAMBIGUATE      = 1 << 0;
        const REPORT_EVENT_TYPES = 1 << 1;
        const REPORT_ALL_AS_ESC = 1 << 2;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyEvent {
    pub action: KeyAction,
    /// The key itself, unshifted: `shift+a` is `Char('a')`, and the `A` the
    /// platform produced belongs in `text`.
    pub key: Option<Key>,
    pub mods: Modifiers,
    /// Modifiers the platform already spent producing `text`. On layouts where
    /// a character needs alt (or AltGr), alt must not also be encoded as meta,
    /// or `alt+8` on a Nordic layout sends meta instead of the `[` it printed.
    pub consumed_mods: Modifiers,
    /// What the platform produced for this key, after any dead-key or input
    /// method composition. Empty for keys that produce no text.
    pub text: Option<String>,
    /// True while an input method owns the key. Nothing is encoded: the text
    /// arrives later, when composition commits.
    pub composing: bool,
}

/// Terminal state the encoding depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncodeContext {
    /// DECCKM. Arrows and Home/End switch from `CSI` to `SS3`.
    pub app_cursor: bool,
    pub kitty: KittyFlags,
    /// `CSI > 4 ; n m`. Levels 1 and 2 are treated alike: both ask for
    /// unambiguous reporting of modified keys, and the difference between them
    /// only concerns keys this encoder already reports unambiguously.
    pub modify_other_keys: Option<u8>,
    /// Whether alt should act as meta, prefixing with ESC, instead of letting
    /// the platform's text through. Terminals default this on; on macOS it is
    /// the difference between `alt+d` deleting a word and inserting `∂`.
    pub alt_is_meta: bool,
}

impl Modifiers {
    /// The modifier parameter shared by CSI sequences, kitty included: a
    /// 1-based bitfield where shift is 1, alt 2, ctrl 4 and super 8.
    fn param(self) -> u8 {
        let mut value = 1;
        if self.contains(Modifiers::SHIFT) {
            value += 1;
        }
        if self.contains(Modifiers::ALT) {
            value += 2;
        }
        if self.contains(Modifiers::CTRL) {
            value += 4;
        }
        if self.contains(Modifiers::SUPER) {
            value += 8;
        }
        value
    }
}

impl KeyAction {
    /// Kitty's event type parameter: press is 1 and may be omitted, repeat 2,
    /// release 3.
    fn kitty_param(self) -> u8 {
        match self {
            KeyAction::Press => 1,
            KeyAction::Repeat => 2,
            KeyAction::Release => 3,
        }
    }
}

/// The control byte a key produces with ctrl held, following the ASCII
/// C0 layout that terminals have always used.
fn ctrl_byte(c: char) -> Option<u8> {
    let byte = match c {
        ' ' | '@' => 0x00,
        'a'..='z' => (c as u8) & 0x1f,
        'A'..='Z' => (c as u8) & 0x1f,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        // ctrl+? is DEL, which is how ctrl+backspace is usually spelled.
        '?' => 0x7f,
        // ctrl+2 and ctrl+3 through ctrl+8 alias the block above on most
        // keyboards, because that is where those symbols sit unshifted.
        '2' => 0x00,
        '3' => 0x1b,
        '4' => 0x1c,
        '5' => 0x1d,
        '6' => 0x1e,
        '7' | '/' => 0x1f,
        '8' => 0x7f,
        _ => return None,
    };
    Some(byte)
}

/// When a key's unmodified form uses SS3 rather than CSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ss3 {
    /// Only in application cursor mode: the arrows and Home/End follow DECCKM.
    WhenAppCursor,
    /// Always. F1 to F4 are SS3 by convention, independent of DECCKM.
    Always,
}

/// The final byte of a key's legacy CSI form, if it has one, and when SS3
/// applies to it.
fn csi_final(key: Key) -> Option<(char, Ss3)> {
    let pair = match key {
        Key::Up => ('A', Ss3::WhenAppCursor),
        Key::Down => ('B', Ss3::WhenAppCursor),
        Key::Right => ('C', Ss3::WhenAppCursor),
        Key::Left => ('D', Ss3::WhenAppCursor),
        Key::Home => ('H', Ss3::WhenAppCursor),
        Key::End => ('F', Ss3::WhenAppCursor),
        Key::F(1) => ('P', Ss3::Always),
        Key::F(2) => ('Q', Ss3::Always),
        Key::F(3) => ('R', Ss3::Always),
        Key::F(4) => ('S', Ss3::Always),
        _ => return None,
    };
    Some(pair)
}

/// The number in a key's legacy `CSI n ~` form, if it has one.
fn tilde_number(key: Key) -> Option<u8> {
    let number = match key {
        Key::Insert => 2,
        Key::Delete => 3,
        Key::PageUp => 5,
        Key::PageDown => 6,
        Key::F(5) => 15,
        Key::F(6) => 17,
        Key::F(7) => 18,
        Key::F(8) => 19,
        Key::F(9) => 20,
        Key::F(10) => 21,
        Key::F(11) => 23,
        Key::F(12) => 24,
        _ => return None,
    };
    Some(number)
}

/// The codepoint kitty identifies a key by. Text keys are themselves; the rest
/// use the private range the protocol assigns.
fn kitty_codepoint(key: Key) -> u32 {
    match key {
        Key::Char(c) => c as u32,
        Key::Escape => 27,
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Backspace => 127,
        Key::Insert => 2,
        Key::Delete => 3,
        Key::Left => 57417,
        Key::Right => 57418,
        Key::Up => 57419,
        Key::Down => 57420,
        Key::PageUp => 57421,
        Key::PageDown => 57422,
        Key::Home => 57423,
        Key::End => 57424,
        Key::F(n) => 57363 + n as u32 - 1,
    }
}

fn push_mods_and_event(out: &mut String, mods: Modifiers, action: KeyAction, kitty: KittyFlags) {
    let report_event = kitty.contains(KittyFlags::REPORT_EVENT_TYPES) && action != KeyAction::Press;
    if mods.is_empty() && !report_event {
        return;
    }
    out.push(';');
    out.push_str(&mods.param().to_string());
    if report_event {
        out.push(':');
        out.push_str(&action.kitty_param().to_string());
    }
}

/// Encode in kitty's form: functional keys keep their legacy shape with the
/// modifier parameter, everything else becomes `CSI codepoint ; mods u`.
fn encode_kitty(key: Key, mods: Modifiers, action: KeyAction, ctx: &EncodeContext) -> Vec<u8> {
    let mut out = String::from("\x1b[");
    if let Some(number) = tilde_number(key) {
        out.push_str(&number.to_string());
        push_mods_and_event(&mut out, mods, action, ctx.kitty);
        out.push('~');
        return out.into_bytes();
    }

    if let Some((final_byte, _)) = csi_final(key) {
        // A modifier or event type needs the leading 1 so the parameters line
        // up: `CSI 1 ; 5 A` rather than `CSI ; 5 A`.
        let has_params = !mods.is_empty()
            || (ctx.kitty.contains(KittyFlags::REPORT_EVENT_TYPES) && action != KeyAction::Press);
        if has_params {
            out.push('1');
        }
        push_mods_and_event(&mut out, mods, action, ctx.kitty);
        out.push(final_byte);
        return out.into_bytes();
    }

    out.push_str(&kitty_codepoint(key).to_string());
    push_mods_and_event(&mut out, mods, action, ctx.kitty);
    out.push('u');
    out.into_bytes()
}

/// `CSI 27 ; mods ; codepoint ~`, xterm's unambiguous form for a modified key.
fn encode_modify_other_keys(codepoint: u32, mods: Modifiers) -> Vec<u8> {
    format!("\x1b[27;{};{}~", mods.param(), codepoint).into_bytes()
}

/// The legacy encoding: what terminals send with no keyboard protocol active.
fn encode_legacy(
    key: Key,
    mods: Modifiers,
    text: Option<&str>,
    ctx: &EncodeContext,
) -> Option<Vec<u8>> {
    // Any modifier at all takes the parameterised CSI form for keys that have
    // one, which is also what unmodified keys use outside application mode.
    if let Some((final_byte, ss3)) = csi_final(key) {
        let bytes = if mods.is_empty() {
            if ss3 == Ss3::Always || ctx.app_cursor {
                format!("\x1bO{final_byte}")
            } else {
                format!("\x1b[{final_byte}")
            }
        } else {
            format!("\x1b[1;{}{final_byte}", mods.param())
        };
        return Some(bytes.into_bytes());
    }

    if let Some(number) = tilde_number(key) {
        let bytes = if mods.is_empty() {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{}~", mods.param())
        };
        return Some(bytes.into_bytes());
    }

    let alt = mods.contains(Modifiers::ALT);
    let ctrl = mods.contains(Modifiers::CTRL);

    let base: Vec<u8> = match key {
        Key::Enter => vec![b'\r'],
        Key::Tab => {
            if mods.contains(Modifiers::SHIFT) {
                return Some(b"\x1b[Z".to_vec());
            }
            vec![b'\t']
        }
        Key::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        Key::Escape => vec![0x1b],
        Key::Char(c) => {
            if (c as u32) >= 0x10 && (c as u32) < 0x20 {
                // Slint maps bare modifier presses to control chars in
                // event.text (Shift → DLE 0x10, Control → 0x11 Alt → 0x12 …).
                // Forwarding these to the pty makes shells interpret them as
                // live input (e.g. Shift alone → Ctrl-P = previous history).
                return None;
            }
            if ctrl {
                // ctrl+key that has no control byte sends nothing legacy can
                // express; `modifyOtherKeys` or kitty mode is how a program
                // asks to see it.
                vec![ctrl_byte(c)?]
            } else if let Some(text) = text.filter(|t| !t.is_empty()) {
                // Prefer what the platform produced: it already accounts for
                // shift, the layout, and any dead key.
                text.as_bytes().to_vec()
            } else {
                c.to_string().into_bytes()
            }
        }
        // Handled above.
        Key::Up
        | Key::Down
        | Key::Left
        | Key::Right
        | Key::Home
        | Key::End
        | Key::Insert
        | Key::Delete
        | Key::PageUp
        | Key::PageDown
        | Key::F(_) => return None,
    };

    if alt && ctx.alt_is_meta {
        let mut bytes = Vec::with_capacity(base.len() + 1);
        bytes.push(0x1b);
        bytes.extend_from_slice(&base);
        return Some(bytes);
    }

    Some(base)
}

/// Whether the legacy form of this key loses information the terminal asked to
/// see: ctrl or alt combined with a key whose control byte is ambiguous or
/// absent, and Escape, which is indistinguishable from the start of any
/// sequence.
fn needs_disambiguation(key: Key, mods: Modifiers) -> bool {
    if mods.contains(Modifiers::CTRL) || mods.contains(Modifiers::ALT) {
        return true;
    }
    matches!(key, Key::Escape)
}

pub fn encode(event: &KeyEvent, ctx: &EncodeContext) -> Option<Vec<u8>> {
    // An input method owns the key. Its text arrives on commit.
    if event.composing {
        return None;
    }

    // No key: a text-only event, such as an input method committing a
    // composition. The composed text goes to the program as-is; there is
    // no keystroke to encode.
    let Some(key) = event.key else {
        if event.action != KeyAction::Press {
            return None;
        }
        return event
            .text
            .clone()
            .filter(|text| !text.is_empty())
            .map(String::into_bytes);
    };

    // Releases exist only for programs that asked for event types.
    if event.action == KeyAction::Release && !ctx.kitty.contains(KittyFlags::REPORT_EVENT_TYPES) {
        return None;
    }

    // A modifier the platform spent on producing the text is not also a
    // modifier the program should see.
    let mods = event.mods.difference(event.consumed_mods);
    let text = event.text.as_deref();

    let kitty_active = !ctx.kitty.is_empty();
    if kitty_active {
        let all = ctx.kitty.contains(KittyFlags::REPORT_ALL_AS_ESC);
        let event_types =
            ctx.kitty.contains(KittyFlags::REPORT_EVENT_TYPES) && event.action != KeyAction::Press;
        let disambiguate =
            ctx.kitty.contains(KittyFlags::DISAMBIGUATE) && needs_disambiguation(key, mods);

        if all || event_types || disambiguate {
            return Some(encode_kitty(key, mods, event.action, ctx));
        }
    }

    // Text keys with a modifier the legacy form mangles, when the program has
    // asked for the unambiguous form.
    if ctx.modify_other_keys.is_some_and(|level| level >= 1)
        && let Key::Char(c) = key
        && (mods.contains(Modifiers::CTRL) || mods.contains(Modifiers::ALT))
    {
        return Some(encode_modify_other_keys(c as u32, mods));
    }

    // Super is the platform's own modifier: a terminal has nothing to send for
    // it, and swallowing it here keeps `cmd+c` from reaching the shell as `c`.
    if mods.contains(Modifiers::SUPER) {
        return None;
    }

    encode_legacy(key, mods, text, ctx)
}
