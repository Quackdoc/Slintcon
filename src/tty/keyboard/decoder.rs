//! The `K_MEDIUMRAW` byte stream.

/// Splits the `K_MEDIUMRAW` byte stream into keycode edges.
#[derive(Default)]
pub(super) struct Decoder {
    /// Inside a `0, hi, lo` sequence: whether it is a release, and the high
    /// part once seen.
    extended: Option<(bool, Option<u16>)>,
}

impl Decoder {
    pub(super) fn feed(&mut self, bytes: &[u8], mut edge: impl FnMut(u16, bool)) {
        for &byte in bytes {
            self.extended = match self.extended {
                None if byte & 0x7f == 0 => Some((byte & 0x80 != 0, None)),
                None => {
                    edge(u16::from(byte & 0x7f), byte & 0x80 == 0);
                    None
                }
                Some((up, None)) => Some((up, Some(u16::from(byte & 0x7f)))),
                Some((up, Some(high))) => {
                    edge(high << 7 | u16::from(byte & 0x7f), !up);
                    None
                }
            };
        }
    }
}
