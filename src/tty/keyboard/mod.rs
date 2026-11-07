//! tty-style keyboard input: keys come from the VT, not from evdev.
//!
//! The VT keyboard goes into `K_MEDIUMRAW` and the tty into raw termios, so the
//! kernel delivers keycodes only while our VT is active and nobody put the
//! keyboard into `K_OFF` (a compositor started from here does). Keycodes are
//! translated with the kernel's own console keymap (`KDGKBENT`, plus the dead
//! keys from `KDGKBDIACRUC`); [`Translator`] yields [`KeyEvent`]s and VT switch
//! requests. See `docs/keyboard-input.md`.

mod decoder;
mod keymap;
mod translator;

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use procfs::process::{FDTarget, Process, Syscall};
use rustix::termios::{self, OptionalActions, QueueSelector, SpecialCodeIndex, Termios};

use super::{CONSOLE_FD, Request, TTY_KEYBOARD, borrow, ioctl_int, ioctl_ptr, switch_to, warn};
use slint_terminal::key::KeyEvent;

use decoder::Decoder;
use keymap::{Keymap, SysKeymap};
use translator::{Output, Translator};

const KDGKBMODE: Request = 0x4B44;
const KDSKBMODE: Request = 0x4B45;
const KDGKBLED: Request = 0x4B64;

const K_MEDIUMRAW: libc::c_int = 0x02;
const K_UNICODE: libc::c_int = 0x03;

/// The termios to put back on exit.
static SAVED_TERMIOS: Mutex<Option<Termios>> = Mutex::new(None);
/// We changed the keyboard mode and termios and must put them back.
static RESTORE_KEYBOARD: AtomicBool = AtomicBool::new(false);
/// The keyboard mode to go back to.
static PRIOR_KBMODE: AtomicI32 = AtomicI32::new(K_UNICODE);

/// Undo [`Keyboard::engage`]. Callable from any thread.
pub(super) fn restore_keyboard() {
    if !RESTORE_KEYBOARD.swap(false, Ordering::SeqCst) {
        return;
    }
    TTY_KEYBOARD.store(false, Ordering::SeqCst);
    let fd = CONSOLE_FD.load(Ordering::SeqCst);
    if fd == -1 {
        return;
    }
    // Only if the mode is still ours: on a normal exit Slint's console code
    // already put back what it found, which is the better answer.
    let mut mode: libc::c_int = 0;
    if ioctl_ptr(fd, KDGKBMODE, &mut mode).is_ok() && mode == K_MEDIUMRAW {
        let _ = ioctl_int(fd, KDSKBMODE, PRIOR_KBMODE.load(Ordering::SeqCst));
    }
    if let Some(saved) = SAVED_TERMIOS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        let _ = termios::tcsetattr(borrow(fd), OptionalActions::Now, saved);
    }
}

/// Syscalls a process blocks in while waiting for input (poll, select, read).
#[cfg(target_arch = "x86_64")]
const WAITING_SYSCALLS: &[u32] = &[0, 7, 23, 232, 270, 271, 281];
#[cfg(target_arch = "aarch64")]
const WAITING_SYSCALLS: &[u32] = &[63, 72, 73, 22, 441];
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const WAITING_SYSCALLS: &[u32] = &[];

/// An ancestor that reads the VT's input too (`sudo` with `use_pty` relays
/// the tty into a pty): it would take the keycodes before we do. Returns
/// `(pid, command)`.
fn competing_reader(vt: u16) -> Option<(u32, String)> {
    let tty = FDTarget::Path(format!("/dev/tty{vt}").into());
    let mut process = Process::myself().ok()?;
    for _ in 0..64 {
        let parent = process.stat().ok()?.ppid;
        if parent <= 1 {
            return None;
        }
        process = Process::new(parent).ok()?;
        let on_tty = (0..=2).any(|fd| process.fd_from_fd(fd).is_ok_and(|info| info.target == tty));
        // Holding the tty is normal (a shell, openvt waiting for its child);
        // blocked in poll/read on it is not.
        let waiting = matches!(
            process.syscall(),
            Ok(Syscall::Blocked { syscall_number, .. })
                if u32::try_from(syscall_number).is_ok_and(|number| WAITING_SYSCALLS.contains(&number))
        );
        if on_tty && waiting {
            let command = process.stat().map(|stat| stat.comm).unwrap_or_default();
            return Some((parent as u32, command));
        }
    }
    None
}

/// The VT keyboard: `K_MEDIUMRAW` on our tty, translated to key events.
pub(super) struct Keyboard {
    /// Non-blocking, separate from the VT-switching fd (`O_NONBLOCK` belongs
    /// to the open file description).
    tty: File,
    translator: Translator,
    decoder: Decoder,
    read_failed: bool,
}

impl Keyboard {
    /// Take the keyboard of `/dev/tty<vt>`. Errors leave everything as found
    /// (the caller then keeps reading evdev).
    pub(super) fn engage(vt: u16) -> io::Result<Self> {
        if let Some((pid, command)) = competing_reader(vt) {
            return Err(io::Error::other(format!(
                "{command} (pid {pid}) also reads tty{vt} and would take the keys; \
                 run slintcon without a process that relays the tty, e.g. through \
                 openvt as in the readme, or with sudo's use_pty off"
            )));
        }
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(format!("/dev/tty{vt}"))?;
        let fd = tty.as_raw_fd();

        let mut prior: libc::c_int = 0;
        ioctl_ptr(fd, KDGKBMODE, &mut prior)?;
        // The kernel hides Unicode keysyms from KDGKBENT outside K_UNICODE.
        ioctl_int(fd, KDSKBMODE, K_UNICODE)?;
        let keymap = match Keymap::load(&SysKeymap(fd)) {
            Ok(keymap) => keymap,
            Err(error) => {
                let _ = ioctl_int(fd, KDSKBMODE, prior);
                return Err(error);
            }
        };

        let mut raw = match termios::tcgetattr(borrow(fd)) {
            Ok(termios) => termios,
            Err(error) => {
                let _ = ioctl_int(fd, KDSKBMODE, prior);
                return Err(error.into());
            }
        };
        *SAVED_TERMIOS.lock().unwrap_or_else(|e| e.into_inner()) = Some(raw.clone());
        PRIOR_KBMODE.store(
            // A mode left behind by a killed run is not worth restoring.
            if prior == K_MEDIUMRAW {
                K_UNICODE
            } else {
                prior
            },
            Ordering::SeqCst,
        );
        RESTORE_KEYBOARD.store(true, Ordering::SeqCst);

        // Keycodes are not text: no line buffering, signals, echo or flow
        // control (`^C` is the keycode of `2`).
        raw.make_raw();
        raw.special_codes[SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[SpecialCodeIndex::VTIME] = 0;
        let configured = termios::tcsetattr(borrow(fd), OptionalActions::Now, &raw)
            .map_err(io::Error::from)
            .and_then(|()| ioctl_int(fd, KDSKBMODE, K_MEDIUMRAW));
        if let Err(error) = configured {
            restore_keyboard();
            return Err(error);
        }
        let _ = termios::tcflush(borrow(fd), QueueSelector::IFlush);
        TTY_KEYBOARD.store(true, Ordering::SeqCst);
        Ok(Self {
            tty,
            translator: Translator::new(keymap),
            decoder: Decoder::default(),
            read_failed: false,
        })
    }

    /// The modifiers held right now. Bare modifier keys produce no events,
    /// so this is the only way to learn about them (for the mouse).
    pub(super) fn modifiers(&self) -> slint_terminal::key::Modifiers {
        self.translator.mods()
    }

    /// Drop held keys and unread input, e.g. while another VT is shown.
    pub(super) fn release_all(&mut self) {
        self.translator.reset();
        self.decoder = Decoder::default();
    }

    /// Read what arrived since the last call.
    pub(super) fn poll(&mut self) -> Vec<KeyEvent> {
        let fd = self.tty.as_raw_fd();
        // Someone (a compositor that exited) put the mode back: take it
        // again, and forget what we saw before.
        let mut mode: libc::c_int = 0;
        if ioctl_ptr(fd, KDGKBMODE, &mut mode).is_ok() && mode != K_MEDIUMRAW {
            if let Err(error) = ioctl_int(fd, KDSKBMODE, K_MEDIUMRAW) {
                if !std::mem::replace(&mut self.read_failed, true) {
                    warn(format_args!("cannot take the VT keyboard: {error}"));
                }
                return Vec::new();
            }
            self.read_failed = false;
            self.release_all();
            let _ = termios::tcflush(borrow(fd), QueueSelector::IFlush);
            return Vec::new();
        }

        let mut events = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let read = match self.tty.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    if error.kind() != io::ErrorKind::WouldBlock
                        && !std::mem::replace(&mut self.read_failed, true)
                    {
                        warn(format_args!("cannot read the VT keyboard: {error}"));
                    }
                    break;
                }
            };
            let mut edges = Vec::new();
            self.decoder
                .feed(&buffer[..read], |code, down| edges.push((code, down)));
            for (code, down) in edges {
                match self.translator.key(code, down, self.leds()) {
                    Some(Output::Key(event)) => {
                        log::debug!("vt key {code} down={down}: {event:?}");
                        events.push(event);
                    }
                    Some(Output::Switch(vt)) => {
                        if let Err(error) = switch_to(vt) {
                            warn(format_args!("could not switch to VT {vt}: {error}"));
                        }
                    }
                    None => {}
                }
            }
        }
        events
    }

    fn leds(&self) -> u8 {
        let mut leds: u8 = 0;
        if ioctl_ptr(self.tty.as_raw_fd(), KDGKBLED, &mut leds).is_err() {
            return 0;
        }
        leds & 0x07
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        restore_keyboard();
    }
}
