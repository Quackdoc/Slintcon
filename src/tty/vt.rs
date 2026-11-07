//! `VT_PROCESS` switching for the LinuxKMS backend, taken over from Slint's
//! console setup on the first timer tick. The signal handlers only set
//! atomics and the tick does the work on the main thread, which must also be
//! the one that calls `VT_SETMODE`. See `docs/vt-switching.md`.

use std::io;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use slint_terminal::key::KeyEvent;
use std::sync::atomic::{AtomicBool, Ordering};

use super::clients::DisplayClients;
use super::drm_master::DrmMaster;
use super::keyboard::{Keyboard, restore_keyboard};
use super::{
    CONSOLE_FD, DISPLAY_ACTIVE, Request, VtChange, active_vt, ioctl_int, ioctl_ptr, open_tty, warn,
};

const VT_GETMODE: Request = 0x5601;
const VT_SETMODE: Request = 0x5602;
const VT_RELDISP: Request = 0x5605;
const KDSETMODE: Request = 0x4B3A;
const KDGETMODE: Request = 0x4B3B;
const KD_TEXT: libc::c_int = 0;
const KD_GRAPHICS: libc::c_int = 1;
const VT_AUTO: libc::c_char = 0;
const VT_PROCESS: libc::c_char = 1;
/// `VT_RELDISP` argument: allow the pending release.
const RELEASE_OK: libc::c_int = 1;
const RELEASE_SIGNAL: libc::c_int = libc::SIGUSR1;
const ACQUIRE_SIGNAL: libc::c_int = libc::SIGUSR2;
/// Upstream sets its mode once; a few re-takes cover that, more means a loop.
const MAX_TAKEOVERS: u32 = 8;

static RELEASE_REQUESTED: LazyLock<Arc<AtomicBool>> = LazyLock::new(Arc::default);
static ACQUIRE_REQUESTED: LazyLock<Arc<AtomicBool>> = LazyLock::new(Arc::default);
/// We switched the VT away from `VT_AUTO`; nobody else will switch it back.
/// How long a signalled exit may take before it is forced.
const EXIT_GRACE: Duration = Duration::from_secs(3);

static RESTORE_VT_MODE: AtomicBool = AtomicBool::new(false);
/// We are responsible for putting the VT back into `KD_TEXT`.
static RESTORE_TEXT_MODE: AtomicBool = AtomicBool::new(false);

/// `struct vt_mode` from `<linux/vt.h>`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct VtMode {
    mode: libc::c_char,
    waitv: libc::c_char,
    relsig: libc::c_short,
    acqsig: libc::c_short,
    frsig: libc::c_short,
}

impl VtMode {
    fn ours() -> Self {
        Self {
            mode: VT_PROCESS,
            relsig: RELEASE_SIGNAL as libc::c_short,
            acqsig: ACQUIRE_SIGNAL as libc::c_short,
            ..Self::default()
        }
    }
}

/// Owns VT switching for the process; see the module documentation.
pub struct VtSession {
    state: SessionState,
    /// Keys read from the VT once engaged; `None` falls back to evdev.
    keyboard: Option<Keyboard>,
    tty_keyboard: bool,
    /// `--vt N`: the VT we were told to run on.
    expected_vt: Option<u16>,
    /// The active VT was not `expected_vt` when we engaged (the user switched
    /// away right after the wait ended): the event loop should quit so the
    /// supervisor starts us again.
    wrong_vt: bool,
}

/// The error for [`engage`] when the active VT is not the expected one.
#[derive(Debug)]
struct WrongVt {
    active: u16,
    expected: u16,
}

impl std::fmt::Display for WrongVt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tty{} is active but --vt {} was requested",
            self.active, self.expected
        )
    }
}

impl std::error::Error for WrongVt {}

enum SessionState {
    Disabled,
    /// Waiting for the first tick inside the event loop, after Slint's
    /// console setup.
    Pending,
    Engaged(Switcher),
}

impl VtSession {
    /// `linuxkms`: only the LinuxKMS backend touches the VT. `tty_keyboard`:
    /// read keys from the VT instead of evdev. `expected_vt`: the VT `--vt`
    /// asked for; engaging on another one fails (see [`Self::wants_exit`]).
    pub fn new(linuxkms: bool, tty_keyboard: bool, expected_vt: Option<u16>) -> Self {
        let state = if linuxkms {
            // Before Slint installs its own handlers (it skips signals that
            // already have one), so a signal reaches us first.
            install_exit_handlers();
            SessionState::Pending
        } else {
            SessionState::Disabled
        };
        Self {
            state,
            keyboard: None,
            tty_keyboard,
            expected_vt,
            wrong_vt: false,
        }
    }

    /// True when the event loop should quit: we engaged on the wrong VT.
    pub fn wants_exit(&self) -> bool {
        self.wrong_vt
    }

    /// Call from the event loop's timer. Returns what the terminal has to do.
    pub fn poll(&mut self) -> Option<VtChange> {
        if let SessionState::Pending = self.state {
            let engaged = engage(self.expected_vt, self.tty_keyboard);
            self.wrong_vt = engaged
                .as_ref()
                .err()
                .and_then(|error| error.get_ref())
                .is_some_and(|inner| inner.is::<WrongVt>());
            (self.state, self.keyboard) = match engaged {
                Ok((switcher, keyboard)) => (SessionState::Engaged(switcher), keyboard),
                Err(error) => {
                    warn(format_args!("VT switching is unavailable: {error}"));
                    (SessionState::Disabled, None)
                }
            };
        }
        let SessionState::Engaged(switcher) = &mut self.state else {
            return None;
        };
        // Acquire first: if both are pending, the release came last.
        let requests = Requests {
            acquire: ACQUIRE_REQUESTED.swap(false, Ordering::SeqCst),
            release: RELEASE_REQUESTED.swap(false, Ordering::SeqCst),
        };
        let change = switcher.poll(requests);
        if let Some(change) = change {
            DISPLAY_ACTIVE.store(change == VtChange::Resume, Ordering::SeqCst);
        }
        change
    }
}

impl VtSession {
    /// Whether the VT is being polled (switching, or keys read from it), which
    /// needs the fast UI tick.
    pub fn polls(&self) -> bool {
        !matches!(self.state, SessionState::Disabled)
    }

    /// Key events read from the VT since the last call (empty while another
    /// VT, or a compositor started from here, owns the display).
    /// The modifiers held on the VT keyboard, or `None` when keys are not
    /// read from it (evdev, or another VT is shown).
    pub fn tty_modifiers(&self) -> Option<slint_terminal::key::Modifiers> {
        match (&self.state, &self.keyboard) {
            (SessionState::Engaged(switcher), Some(keyboard))
                if switcher.state == State::Active =>
            {
                Some(keyboard.modifiers())
            }
            _ => None,
        }
    }

    pub fn poll_keys(&mut self) -> Vec<KeyEvent> {
        let (SessionState::Engaged(switcher), Some(keyboard)) = (&self.state, &mut self.keyboard)
        else {
            return Vec::new();
        };
        if switcher.state != State::Active {
            keyboard.release_all();
            return Vec::new();
        }
        keyboard.poll()
    }
}

impl Drop for VtSession {
    fn drop(&mut self) {
        self.keyboard = None;
        if let SessionState::Engaged(switcher) = &mut self.state {
            switcher.shutdown(RELEASE_REQUESTED.swap(false, Ordering::SeqCst));
        }
    }
}

/// The active VT. `/dev/tty0` is root-only on most systems, while the sysfs
/// file is world-readable, so it is tried first: an unprivileged user on their
/// own VT can then run everything except what needs the console capability.
fn current_vt() -> io::Result<u16> {
    let from_sysfs = std::fs::read_to_string("/sys/class/tty/tty0/active")
        .ok()
        .and_then(|text| text.trim().strip_prefix("tty")?.parse().ok());
    if let Some(vt) = from_sysfs {
        return Ok(vt);
    }
    let control = open_tty("/dev/tty0")?;
    active_vt(control.as_raw_fd())
}

/// Take over the VT and, with `tty_keyboard`, its keyboard; the keyboard is
/// `None` when it is not wanted or cannot be read (evdev is used instead).
fn engage(
    expected_vt: Option<u16>,
    tty_keyboard: bool,
) -> io::Result<(Switcher, Option<Keyboard>)> {
    let vt = current_vt()?;
    if let Some(expected) = expected_vt.filter(|&expected| expected != vt) {
        return Err(io::Error::other(WrongVt {
            active: vt,
            expected,
        }));
    }
    let tty = open_tty(&format!("/dev/tty{vt}"))?;
    // Handlers before VT_SETMODE: the default action of SIGUSR1 kills us.
    install_switch_handlers()?;
    // Never closed: the exit signal handler and the hotkey use it.
    let fd = tty.into_raw_fd();
    CONSOLE_FD.store(fd, Ordering::SeqCst);
    info(format_args!("handling VT switches on tty{vt}"));
    let keyboard = tty_keyboard
        .then(|| Keyboard::engage(vt))
        .and_then(|result| {
            result
                .inspect(|_| info(format_args!("reading the keyboard from tty{vt}")))
                .inspect_err(|error| {
                    warn(format_args!(
                        "cannot read the keyboard from tty{vt} ({error}); using the Slint keyboard"
                    ))
                })
                .ok()
        });
    Ok((Switcher::new(fd, vt), keyboard))
}

#[derive(Clone, Copy, Default, Debug)]
struct Requests {
    release: bool,
    acquire: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// We are shown and draw.
    Active,
    /// A release was requested; drawing stopped, ack on the next poll.
    Releasing,
    /// Another VT is shown (or we can't get DRM master back yet).
    Released,
    /// Another process took over switching on our VT after we did, i.e. a
    /// compositor started from our shell. Drawing and input are off until it
    /// gives the VT back.
    Foreign,
}

/// What to do about the VT mode found on our VT.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModeAction {
    Keep,
    /// Set our mode. `upstream_restores`: Slint's console code set the
    /// current mode and will restore text mode and `VT_AUTO` itself.
    Take {
        upstream_restores: bool,
    },
    /// Another process controls switching on this VT.
    Yield,
}

fn mode_action(current: VtMode, registered: bool) -> ModeAction {
    match current.mode {
        VT_AUTO => ModeAction::Take {
            upstream_restores: false,
        },
        VT_PROCESS if current.relsig == 0 => ModeAction::Take {
            upstream_restores: true,
        },
        // Same signals but not set by us (yet): a killed earlier run. The
        // kernel only knows the dead owner, so register again.
        VT_PROCESS if current == VtMode::ours() && !registered => ModeAction::Take {
            upstream_restores: false,
        },
        VT_PROCESS if current == VtMode::ours() => ModeAction::Keep,
        _ => ModeAction::Yield,
    }
}

struct Switcher {
    /// Our VT's tty, never closed.
    fd: RawFd,
    drm: DrmMaster,
    clients: DisplayClients,
    vt: u16,
    state: State,
    /// False once we yielded to another owner or gave up.
    managing: bool,
    /// We set our mode at least once.
    registered: bool,
    /// DRM master was dropped for the current [`State::Foreign`] period.
    foreign_master_dropped: bool,
    takeovers: u32,
}

impl Switcher {
    fn new(fd: RawFd, vt: u16) -> Self {
        Self {
            fd,
            drm: DrmMaster::default(),
            clients: DisplayClients::default(),
            vt,
            state: State::Active,
            managing: true,
            registered: false,
            foreign_master_dropped: false,
            takeovers: 0,
        }
    }

    fn poll(&mut self, requests: Requests) -> Option<VtChange> {
        let was_drawing = self.state == State::Active;
        let mut acked_release = false;
        if self.state == State::Releasing {
            self.finish_release();
            acked_release = true;
        }
        if requests.acquire {
            self.acquired();
        }
        if requests.release {
            match self.state {
                State::Active => self.state = State::Releasing,
                State::Releasing => {}
                // Signals still reach us if the other owner did not replace
                // our mode (seatd uses the same ones): don't block the switch.
                State::Foreign => self.ack_release(),
                // Not drawing anyway; don't keep the kernel waiting.
                State::Released => self.ack_release(),
            }
        }
        if self.state == State::Foreign {
            self.follow_foreign();
        }
        if !acked_release && self.state != State::Releasing {
            self.follow_foreground();
        }
        if self.state == State::Active && self.clients.any() {
            info(format_args!(
                "a program started from the shell opened the display; suspending until it exits"
            ));
            self.foreign_master_dropped = false;
            self.state = State::Foreign;
        }
        if self.state == State::Active && self.managing {
            self.keep_mode();
        }
        let drawing = self.state == State::Active;
        match (was_drawing, drawing) {
            (true, false) => Some(VtChange::Suspend),
            (false, true) => Some(VtChange::Resume),
            _ => None,
        }
    }

    fn finish_release(&mut self) {
        self.drm.drop_master();
        self.ack_release();
        self.state = State::Released;
    }

    fn ack_release(&mut self) {
        if let Err(error) = ioctl_int(self.fd, VT_RELDISP, RELEASE_OK) {
            warn(format_args!("cannot release tty{}: {error}", self.vt));
        }
    }

    fn acquired(&mut self) {
        // No VT_RELDISP(VT_ACKACQ): the kernel ignores it unless a release is
        // pending, and then it completes that release, which could switch
        // away before we stopped drawing.
        if self.state == State::Released {
            self.resume();
        }
    }

    fn resume(&mut self) {
        if self.drm.set_master() {
            if fbdev() {
                force_fbdev_mode();
            }
            self.state = State::Active;
        }
    }

    /// Another owner controls our VT: drop DRM master (a tick after drawing
    /// stopped, so a frame Slint already scheduled still renders), and watch
    /// for the VT being given back.
    fn follow_foreign(&mut self) {
        if !self.foreign_master_dropped {
            self.drm.drop_master();
            self.foreign_master_dropped = true;
            return;
        }
        let Ok(mode) = vt_mode(self.fd) else {
            return;
        };
        if mode_action(mode, self.registered) == ModeAction::Yield || self.clients.any() {
            return;
        }
        // Back to `VT_AUTO` (or similar): the compositor exited. Treat it
        // like a switch back to our VT; `follow_foreground` resumes if we are
        // shown and `keep_mode` takes the mode again.
        info(format_args!("the other display owner is gone; resuming"));
        self.foreign_master_dropped = false;
        self.takeovers = 0;
        self.state = State::Released;
    }

    /// Catch switches that bypassed the protocol, and retry `set_master`.
    fn follow_foreground(&mut self) {
        let Ok(active) = active_vt(self.fd) else {
            return;
        };
        let shown = active == self.vt;
        match self.state {
            State::Active if !shown => {
                self.drm.drop_master();
                self.state = State::Released;
            }
            State::Released if shown => self.resume(),
            _ => {}
        }
    }

    /// Make sure our VT is in our `VT_PROCESS` mode and `KD_GRAPHICS`.
    fn keep_mode(&mut self) {
        let Ok(mode) = vt_mode(self.fd) else {
            return;
        };
        match mode_action(mode, self.registered) {
            ModeAction::Keep => {}
            ModeAction::Yield if self.registered => {
                info(format_args!(
                    "another process took over tty{}; suspending until it exits",
                    self.vt
                ));
                // It restores the mode itself; don't clobber it on exit.
                self.set_restore(false, false);
                self.foreign_master_dropped = false;
                self.state = State::Foreign;
                return;
            }
            ModeAction::Yield => {
                warn(format_args!(
                    "another process controls VT switching on tty{}; leaving it alone",
                    self.vt
                ));
                self.managing = false;
                self.set_restore(false, false);
                return;
            }
            ModeAction::Take { upstream_restores } => {
                if self.takeovers == MAX_TAKEOVERS {
                    warn(format_args!(
                        "the VT mode of tty{} keeps changing; giving up VT switching",
                        self.vt
                    ));
                    self.managing = false;
                    return;
                }
                self.takeovers += 1;
                // Graphics mode goes with the VT mode (KD_GRAPHICS + VT_AUTO
                // refuses every switch). Recorded before the ioctl so an exit
                // signal in between still restores.
                let ours = !upstream_restores;
                self.set_restore(ours, ours);
                if let Err(error) = set_vt_mode(self.fd, &VtMode::ours()) {
                    warn(format_args!(
                        "cannot take VT switching on tty{}: {error}",
                        self.vt
                    ));
                    self.managing = false;
                    self.set_restore(false, false);
                    return;
                }
                self.registered = true;
            }
        }
        // fbcon must not draw while we own the display, and leaving
        // KD_GRAPHICS makes it force its mode back on the next VT (KMS).
        if kd_mode(self.fd).is_ok_and(|mode| mode != KD_GRAPHICS)
            && let Err(error) = ioctl_int(self.fd, KDSETMODE, KD_GRAPHICS)
        {
            warn(format_args!(
                "cannot switch tty{} to graphics mode: {error}",
                self.vt
            ));
        }
    }

    fn set_restore(&mut self, vt_mode: bool, text_mode: bool) {
        RESTORE_VT_MODE.store(vt_mode, Ordering::SeqCst);
        RESTORE_TEXT_MODE.store(text_mode, Ordering::SeqCst);
    }

    /// Exit: let a pending switch complete, then restore what we own.
    fn shutdown(&mut self, release_requested: bool) {
        if release_requested || self.state == State::Releasing {
            self.finish_release();
        }
        restore_console();
    }
}

fn vt_mode(fd: RawFd) -> io::Result<VtMode> {
    let mut mode = VtMode::default();
    ioctl_ptr(fd, VT_GETMODE, &mut mode)?;
    Ok(mode)
}

fn kd_mode(fd: RawFd) -> io::Result<libc::c_int> {
    let mut mode: libc::c_int = 0;
    ioctl_ptr(fd, KDGETMODE, &mut mode)?;
    Ok(mode)
}

fn set_vt_mode(fd: RawFd, mode: &VtMode) -> io::Result<()> {
    let mut mode = *mode;
    ioctl_ptr(fd, VT_SETMODE, &mut mode)
}

/// Async-signal-safe: atomics and ioctls only.
fn restore_console() {
    restore_keyboard();
    let fd = CONSOLE_FD.load(Ordering::SeqCst);
    if fd == -1 {
        return;
    }
    // Text mode first: with KD_GRAPHICS + VT_AUTO the kernel refuses switches.
    if RESTORE_TEXT_MODE.swap(false, Ordering::SeqCst) {
        let _ = ioctl_int(fd, KDSETMODE, KD_TEXT);
    }
    if RESTORE_VT_MODE.swap(false, Ordering::SeqCst) {
        let _ = set_vt_mode(fd, &VtMode::default());
    }
}

/// The kernel's release and acquire requests. The handlers only set flags;
/// `signal-hook` installs them with `SA_RESTART`, which keeps the PTY
/// reader's blocking calls from failing.
fn install_switch_handlers() -> io::Result<()> {
    signal_hook::flag::register(RELEASE_SIGNAL, Arc::clone(&RELEASE_REQUESTED))?;
    signal_hook::flag::register(ACQUIRE_SIGNAL, Arc::clone(&ACQUIRE_REQUESTED))?;
    Ok(())
}

/// Quit the event loop on SIGINT, SIGTERM and SIGHUP, so the normal exit path
/// restores the console. If that does not happen (no event loop yet, a second
/// signal, or no exit within `EXIT_GRACE`), restore here and die by the signal.
fn install_exit_handlers() {
    if std::env::var_os("SLINT_KMS_NO_SIGNAL_HANDLER").is_some() {
        return;
    }
    let mut signals = match Signals::new([SIGINT, SIGTERM, SIGHUP]) {
        Ok(signals) => signals,
        Err(error) => {
            warn(format_args!("cannot handle exit signals: {error}"));
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("exit-signals".into())
        .spawn(move || {
            let mut quitting = false;
            for signal in signals.forever() {
                if !quitting
                    && slint::invoke_from_event_loop(|| _ = slint::quit_event_loop()).is_ok()
                {
                    quitting = true;
                    std::thread::spawn(move || {
                        std::thread::sleep(EXIT_GRACE);
                        die_by(signal);
                    });
                } else {
                    die_by(signal);
                }
            }
        });
    if let Err(error) = spawned {
        warn(format_args!("cannot handle exit signals: {error}"));
    }
}

/// Restore the console, then terminate by `signal` so `waitpid` reports it.
fn die_by(signal: libc::c_int) {
    restore_console();
    let _ = signal_hook::low_level::emulate_default_handler(signal);
}

const FBIOGET_VSCREENINFO: Request = 0x4600;
const FBIOPUT_VSCREENINFO: Request = 0x4601;
/// `fb_var_screeninfo` is 40 `u32`s; `activate` is the 22nd.
const FB_VAR_WORDS: usize = 40;
const FB_VAR_ACTIVATE: usize = 21;
const FB_ACTIVATE_FORCE: u32 = 128;

/// Whether Slint draws to `/dev/fb*` (see `--fbdev`).
fn fbdev() -> bool {
    std::env::var_os("SLINT_BACKEND_LINUXFB").is_some()
}

/// Make the kernel re-apply the fbdev mode. With the DRM fbdev emulation the
/// display keeps what the previous owner (a compositor) left, because fbcon
/// does not restore the mode for a `KD_GRAPHICS` VT; writing the unchanged
/// variable screen info with `FB_ACTIVATE_FORCE` runs `fb_set_par`, which does.
fn force_fbdev_mode() {
    for number in 0..10 {
        let Ok(device) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/dev/fb{number}"))
        else {
            continue;
        };
        let fd = device.as_raw_fd();
        let mut var = [0u32; FB_VAR_WORDS];
        let result = ioctl_ptr(fd, FBIOGET_VSCREENINFO, &mut var).and_then(|()| {
            var[FB_VAR_ACTIVATE] |= FB_ACTIVATE_FORCE;
            ioctl_ptr(fd, FBIOPUT_VSCREENINFO, &mut var)
        });
        match result {
            Ok(()) => info(format_args!("re-applied the mode of /dev/fb{number}")),
            Err(error) => warn(format_args!("cannot re-apply /dev/fb{number}: {error}")),
        }
        return;
    }
}

fn info(message: impl std::fmt::Display) {
    log::info!("slintcon: {message}");
}
