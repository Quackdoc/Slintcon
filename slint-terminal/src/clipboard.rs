//! The clipboard and the primary selection: the system's (arboard) on a
//! desktop, kept in the process where there is no desktop.

/// Which selection a copy or paste addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The explicit clipboard (Ctrl+Shift+C / Ctrl+Shift+V).
    Clipboard,
    /// The highlighted text, pasted with the middle button or Shift+Insert.
    Primary,
}

#[derive(Default)]
pub struct Clipboard {
    system: Option<arboard::Clipboard>,
    /// Connecting is only attempted once; on a console it always fails.
    connected: bool,
    local: [String; 2],
    /// The last copy of this kind never reached the system clipboard (no
    /// desktop, or the compositor lacks that selection), so it is only here.
    local_only: [bool; 2],
}

impl Clipboard {
    /// The system clipboard, connected on first use and then kept: on
    /// Wayland and X11 the contents are served by the process that set them,
    /// so the handle has to outlive the copy.
    fn system(&mut self) -> Option<&mut arboard::Clipboard> {
        if !self.connected {
            self.connected = true;
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.system = Some(clipboard),
                Err(error) => {
                    log::info!("no system clipboard, keeping copies in the process: {error}");
                }
            }
        }
        self.system.as_mut()
    }

    pub fn set(&mut self, kind: Kind, text: String) {
        if text.is_empty() {
            return;
        }
        let synced = match self.system() {
            Some(system) => match set_system(system, kind, &text) {
                Ok(()) => true,
                Err(error) => {
                    log::warn!("cannot set the {kind:?}: {error}");
                    false
                }
            },
            None => false,
        };
        self.local_only[kind as usize] = !synced;
        self.local[kind as usize] = text;
    }

    pub fn get(&mut self, kind: Kind) -> Option<String> {
        let text = if self.local_only[kind as usize] {
            Some(self.local[kind as usize].clone())
        } else {
            match self.system() {
                Some(system) => get_system(system, kind).ok(),
                None => Some(self.local[kind as usize].clone()),
            }
        };
        text.filter(|text| !text.is_empty())
    }
}

fn set_system(
    system: &mut arboard::Clipboard,
    kind: Kind,
    text: &str,
) -> Result<(), arboard::Error> {
    use arboard::SetExtLinux;
    system
        .set()
        .clipboard(linux_kind(kind))
        .text(text.to_owned())
}

fn get_system(system: &mut arboard::Clipboard, kind: Kind) -> Result<String, arboard::Error> {
    use arboard::GetExtLinux;
    system.get().clipboard(linux_kind(kind)).text()
}

fn linux_kind(kind: Kind) -> arboard::LinuxClipboardKind {
    match kind {
        Kind::Clipboard => arboard::LinuxClipboardKind::Clipboard,
        Kind::Primary => arboard::LinuxClipboardKind::Primary,
    }
}
