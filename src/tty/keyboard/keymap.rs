//! The console keymap, read with `KDGKBENT` (tables) and `KDGKBDIACRUC`
//! (dead keys), and the decoding of its entries.

use std::io;
use std::os::fd::RawFd;

use super::{Request, ioctl_ptr};

pub(super) const KDGKBENT: Request = 0x4B46;
pub(super) const KDGKBDIACRUC: Request = 0x4BFA;

pub(super) const NR_KEYS: usize = 256;
pub(super) const MAX_NR_KEYMAPS: usize = 256;
pub(super) const MAX_DIACR: usize = 256;

// Key types (`<linux/keyboard.h>`).
pub(super) const KT_LATIN: u16 = 0;
pub(super) const KT_FN: u16 = 1;
pub(super) const KT_SPEC: u16 = 2;
pub(super) const KT_PAD: u16 = 3;
pub(super) const KT_DEAD: u16 = 4;
pub(super) const KT_CONS: u16 = 5;
pub(super) const KT_CUR: u16 = 6;
pub(super) const KT_SHIFT: u16 = 7;
pub(super) const KT_LETTER: u16 = 11;
pub(super) const KT_CSI: u16 = 15;

pub(super) const K_HOLE: u16 = 0x200;
pub(super) const K_ENTER: u16 = 0x201;
pub(super) const K_NOSUCHMAP: u16 = 0x27f;

/// What the kernel's `ret_diacr` gives for the dead keys it names: grave,
/// acute, circumflex, tilde, diaeresis, cedilla.
pub(super) const DEAD_CHARS: [char; 6] = ['`', '\'', '^', '~', '"', ','];

/// Source of keymap data, behind a trait for the tests.
pub(super) trait KeymapSource {
    /// `KDGKBENT`.
    fn entry(&self, table: u8, keycode: u8) -> io::Result<u16>;
    /// `KDGKBDIACRUC`: `(accent, base, result)` triples.
    fn diacritics(&self) -> io::Result<Vec<(u32, u32, u32)>>;
}

/// The console keymap: one table per modifier combination.
pub(super) struct Keymap {
    tables: Vec<Option<Box<[u16; NR_KEYS]>>>,
    diacritics: Vec<(u32, u32, u32)>,
}

impl Keymap {
    pub(super) fn load(source: &impl KeymapSource) -> io::Result<Self> {
        let mut tables = Vec::with_capacity(MAX_NR_KEYMAPS);
        for index in 0..MAX_NR_KEYMAPS {
            let index = index as u8;
            // K_NOSUCHMAP at keycode 0 means the table does not exist.
            let first = source.entry(index, 0)?;
            if first == K_NOSUCHMAP {
                tables.push(None);
                continue;
            }
            let mut table = Box::new([K_HOLE; NR_KEYS]);
            table[0] = first;
            for keycode in 1..NR_KEYS {
                table[keycode] = source.entry(index, keycode as u8)?;
            }
            tables.push(Some(table));
        }
        // Without it dead keys only produce the bare accent.
        let diacritics = source.diacritics().unwrap_or_default();
        Ok(Self { tables, diacritics })
    }

    pub(super) fn has(&self, table: u16) -> bool {
        self.tables
            .get(usize::from(table))
            .is_some_and(|table| table.is_some())
    }

    pub(super) fn sym(&self, table: u16, keycode: usize) -> u16 {
        self.tables
            .get(usize::from(table))
            .and_then(|table| table.as_ref())
            .map_or(K_HOLE, |table| table[keycode])
    }

    pub(super) fn compose(&self, accent: char, base: char) -> Option<char> {
        self.diacritics
            .iter()
            .find(|&&(a, b, _)| a == accent as u32 && b == base as u32)
            .and_then(|&(_, _, result)| char::from_u32(result))
    }
}

/// A keymap entry, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Sym {
    Hole,
    /// `letter`: CapsLock acts on it.
    Text {
        ch: char,
        letter: bool,
    },
    Dead(u8),
    Fn(u8),
    Cur(u8),
    Pad(u8),
    Enter,
    /// `KG_*`: a modifier key.
    Shift(u8),
    /// Switch to this console (1-based).
    Cons(u8),
    Csi(u8),
    Other,
}

pub(super) fn classify(value: u16) -> Sym {
    // Unicode keysyms are `U+0xxx ^ 0xf000` (the kernel hides them in the
    // type byte).
    if value >= 0xf000 {
        return char::from_u32(u32::from(value ^ 0xf000))
            .filter(|&ch| ch != '\0')
            .map_or(Sym::Hole, |ch| Sym::Text { ch, letter: false });
    }
    let val = (value & 0xff) as u8;
    match value >> 8 {
        KT_LATIN if val == 0 => Sym::Hole,
        // The kernel treats 8-bit keysyms through the console charset;
        // Latin-1 is what every stock keymap means.
        KT_LATIN => Sym::Text {
            ch: char::from(val),
            letter: false,
        },
        KT_LETTER => Sym::Text {
            ch: char::from(val),
            letter: true,
        },
        KT_FN => Sym::Fn(val),
        KT_SPEC if value == K_ENTER => Sym::Enter,
        KT_SPEC if value == K_HOLE || value == K_NOSUCHMAP => Sym::Hole,
        KT_PAD => Sym::Pad(val),
        KT_DEAD => Sym::Dead(val),
        KT_CONS => Sym::Cons(val.saturating_add(1)),
        KT_CUR => Sym::Cur(val),
        KT_SHIFT => Sym::Shift(val),
        KT_CSI => Sym::Csi(val),
        _ => Sym::Other,
    }
}

#[allow(non_snake_case)]
pub(super) const fn K(kind: u16, value: u16) -> u16 {
    kind << 8 | value
}

pub(super) struct SysKeymap(pub(super) RawFd);

impl KeymapSource for SysKeymap {
    fn entry(&self, table: u8, keycode: u8) -> io::Result<u16> {
        #[repr(C)]
        struct KbEntry {
            table: u8,
            index: u8,
            value: u16,
        }
        let mut entry = KbEntry {
            table,
            index: keycode,
            value: 0,
        };
        ioctl_ptr(self.0, KDGKBENT, &mut entry)?;
        Ok(entry.value)
    }

    fn diacritics(&self) -> io::Result<Vec<(u32, u32, u32)>> {
        #[repr(C)]
        struct Diacr {
            diacr: u32,
            base: u32,
            result: u32,
        }
        #[repr(C)]
        struct Diacrs {
            count: u32,
            entries: [Diacr; MAX_DIACR],
        }
        let mut table = Diacrs {
            count: 0,
            entries: std::array::from_fn(|_| Diacr {
                diacr: 0,
                base: 0,
                result: 0,
            }),
        };
        ioctl_ptr(self.0, KDGKBDIACRUC, &mut table)?;
        let count = (table.count as usize).min(MAX_DIACR);
        Ok(table.entries[..count]
            .iter()
            .map(|entry| (entry.diacr, entry.base, entry.result))
            .collect())
    }
}
