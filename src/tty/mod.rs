//! Linux console integration for the LinuxKMS backend.
//!
//! Slint switches the console keyboard off (`K_OFF`), so [`install`] adds a
//! libinput hook that turns Ctrl+Alt+F1..F12 into `VT_ACTIVATE`. The
//! `vt` module implements the `VT_PROCESS` protocol ([`VtSession`]).

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::sys::{Request, borrow, ioctl_int, ioctl_ptr};
use input::event::keyboard::{KeyState, KeyboardEventTrait};

mod clients;
mod drm_master;
mod keyboard;
mod vt;

pub use vt::VtSession;

const VT_GETSTATE: Request = 0x5603;
const VT_ACTIVATE: Request = 0x5606;
const VT_WAITACTIVE: Request = 0x5607;

// Linux evdev key codes. VT switching is a console-level shortcut, so it is
// intentionally keyed on the physical keys rather than the current layout.
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;

/// False while another VT owns the display. Input is then not forwarded to
/// Slint (see [`VtKeyFilter::key`]).
static DISPLAY_ACTIVE: AtomicBool = AtomicBool::new(true);

/// True while the keyboard is read from the VT (see `keyboard`): evdev
/// keyboard events are then ignored, so only the VT's owner receives keys.
static TTY_KEYBOARD: AtomicBool = AtomicBool::new(false);

/// Our VT's tty once `vt` took over switching, else -1. The fd is never
/// closed, so signal handlers can use it at any time.
static CONSOLE_FD: AtomicI32 = AtomicI32::new(-1);

/// What the terminal has to do after a VT switch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VtChange {
    /// Another VT is taking over the display: stop drawing.
    Suspend,
    /// The display is ours again: repaint everything.
    Resume,
}

/// Install the Ctrl+Alt+F1..F12 filter on a LinuxKMS backend selector.
pub fn install(selector: slint::BackendSelector) -> slint::BackendSelector {
    let filter = VtKeyFilter::default();
    selector.with_libinput_event_hook(move |event| filter.handle(event))
}

/// What the hook does with a key event. Returning `true` from the libinput
/// hook makes Slint skip the event entirely.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyAction {
    Forward,
    Consume,
    Switch(u16),
}

#[derive(Default)]
struct VtKeyFilter {
    left_ctrl: Cell<bool>,
    right_ctrl: Cell<bool>,
    left_alt: Cell<bool>,
    right_alt: Cell<bool>,
}

impl VtKeyFilter {
    fn handle(&self, event: &input::Event) -> bool {
        let active = DISPLAY_ACTIVE.load(Ordering::Relaxed);
        let input::Event::Keyboard(event) = event else {
            // Pointer and touch input would make Slint draw its software
            // cursor over the other VT. Device notifications are harmless.
            return !active && !matches!(event, input::Event::Device(_));
        };
        if TTY_KEYBOARD.load(Ordering::Relaxed) {
            // Keys come from the VT instead (`keyboard`), where the kernel
            // routes them to the VT's owner only.
            return true;
        }
        let pressed = event.key_state() == KeyState::Pressed;
        match self.key(event.key(), pressed, active) {
            KeyAction::Forward => false,
            KeyAction::Consume => true,
            KeyAction::Switch(vt) => {
                if let Err(error) = switch_to(vt) {
                    warn(format_args!("could not switch to VT {vt}: {error}"));
                }
                true
            }
        }
    }

    /// Track modifiers and decide what happens to a key event. While another
    /// VT is shown presses are dropped; releases still go to Slint so its
    /// modifier state doesn't stick.
    fn key(&self, key: u32, pressed: bool, active: bool) -> KeyAction {
        let combo = self.update(key, pressed);
        if !active {
            return if pressed {
                KeyAction::Consume
            } else {
                KeyAction::Forward
            };
        }
        combo.map_or(KeyAction::Forward, KeyAction::Switch)
    }

    /// Track modifiers and return the VT to activate for a matching key press.
    fn update(&self, key: u32, pressed: bool) -> Option<u16> {
        match key {
            KEY_LEFTCTRL => self.left_ctrl.set(pressed),
            KEY_RIGHTCTRL => self.right_ctrl.set(pressed),
            KEY_LEFTALT => self.left_alt.set(pressed),
            KEY_RIGHTALT => self.right_alt.set(pressed),
            key => {
                let vt = key_to_vt(key)?;
                let ctrl = self.left_ctrl.get() || self.right_ctrl.get();
                let alt = self.left_alt.get() || self.right_alt.get();
                return (pressed && ctrl && alt).then_some(vt);
            }
        }
        None
    }
}

fn key_to_vt(key: u32) -> Option<u16> {
    match key {
        59..=68 => Some((key - 59 + 1) as u16),
        87 => Some(11),
        88 => Some(12),
        _ => None,
    }
}

/// Ask the kernel to switch VTs. This never waits for the switch: the
/// current VT may be in `VT_PROCESS` mode, and its owner (possibly us, on
/// this very thread) has to acknowledge the release first.
fn switch_to(vt: u16) -> io::Result<()> {
    let tty0;
    let fd = match CONSOLE_FD.load(Ordering::Relaxed) {
        -1 => {
            tty0 = open_tty("/dev/tty0")?;
            tty0.as_raw_fd()
        }
        fd => fd,
    };
    if active_vt(fd).is_ok_and(|active| active == vt) {
        return Ok(());
    }
    ioctl_int(fd, VT_ACTIVATE, libc::c_int::from(vt))
}

/// Block until VT `vt` is the active one (`--vt`): what lets a supervisor start
/// slintcon on a VT that is not shown yet. Slint takes whichever VT is active
/// when its event loop starts, so this has to happen first.
pub fn wait_for_vt(vt: u16) -> io::Result<()> {
    // Any console tty will do; `/dev/tty0` is often root-only.
    let tty = open_tty("/dev/tty0").or_else(|_| open_tty(&format!("/dev/tty{vt}")))?;
    loop {
        match ioctl_int(tty.as_raw_fd(), VT_WAITACTIVE, libc::c_int::from(vt)) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct VtStat {
    v_active: libc::c_ushort,
    v_signal: libc::c_ushort,
    v_state: libc::c_ushort,
}

fn active_vt(fd: RawFd) -> io::Result<u16> {
    let mut state = VtStat::default();
    ioctl_ptr(fd, VT_GETSTATE, &mut state)?;
    Ok(state.v_active)
}

fn open_tty(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
}

fn warn(message: impl std::fmt::Display) {
    log::warn!("slintcon: {message}");
}
