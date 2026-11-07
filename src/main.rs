slint::include_modules!();

mod logging;
// Every `unsafe` block lives here.
#[path = "unsafe.rs"]
mod sys;
mod tty;

use std::cell::RefCell;
use std::ffi::OsString;
use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;

/// VT polling rate while a VT is engaged, and the idle rate.
const POLL_INTERVAL: Duration = Duration::from_millis(16);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Time between the full repaints that follow start-up.
const STARTUP_REPAINT_INTERVAL: Duration = Duration::from_millis(150);
/// How many of them there are (about 2.5 s).
const STARTUP_REPAINTS: u32 = 17;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init();
    log::info!(
        "starting: pid {}, uid {}, args {:?}",
        std::process::id(),
        rustix::process::getuid().as_raw(),
        std::env::args_os().skip(1).collect::<Vec<_>>()
    );
    let result = run();
    match &result {
        Ok(()) => log::info!("exiting normally"),
        Err(error) => log::error!("exiting with an error: {error}"),
    }
    result
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // `slintcon [flags] [-- program [args...]]`: what follows `--` runs in
    // the terminal instead of the user's shell (e.g. `-- /bin/login`).
    let args: Vec<OsString> = std::env::args_os().collect();
    let (flags, command) = match args.iter().position(|arg| arg == "--") {
        Some(split) => (&args[..split], &args[split + 1..]),
        None => (&args[..], &args[args.len()..]),
    };
    let command: Vec<String> = command
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let has = |flag: &str| flags.iter().any(|arg| arg == flag);
    // `--vt N`: wait until VT N is active before starting, then run on it.
    // Accepts `N`, `ttyN` and `/dev/ttyN`, so a dinit service template can pass
    // its instance argument straight in as `--vt $1` (`slintcon@tty6`).
    let requested_vt: Option<u16> = flags
        .iter()
        .position(|arg| arg == "--vt")
        .and_then(|position| flags.get(position + 1))
        .and_then(|value| value.to_str())
        .and_then(|value| {
            let name = value.strip_prefix("/dev/").unwrap_or(value);
            name.strip_prefix("tty").unwrap_or(name).parse().ok()
        });
    if has("--vt") && requested_vt.is_none() {
        return Err("--vt needs a VT number (`N`, `ttyN` or `/dev/ttyN`)".into());
    }
    if let Some(vt) = requested_vt {
        log::info!("waiting for tty{vt} to become active");
        tty::wait_for_vt(vt)?;
        log::info!("tty{vt} is active");
    }

    // fbdev doesn't support GPU rendering.
    let fbdev = has("--fbdev");
    let force_software = fbdev || has("--software") || cfg!(not(feature = "wgpu"));

    let kms = has("--kms") || fbdev;
    if fbdev {
        sys::set_env("SLINT_BACKEND_LINUXFB", "1");
    }

    // Only the LinuxKMS backend may touch the VT; under X11/Wayland the
    // display server owns it.
    let vt = {
        let linuxkms = kms
            || std::env::var("SLINT_BACKEND")
                .is_ok_and(|backend| backend.to_ascii_lowercase().starts_with("linuxkms"));
        Rc::new(RefCell::new(tty::VtSession::new(
            linuxkms,
            !has("--slint-keyboard"),
            requested_vt,
        )))
    };

    let use_cpu = force_software || has("--cpu");
    let mut use_gpu = !use_cpu;

    // A selector can only be used once, so each attempt builds a fresh one.
    // `tty::install` only adds a libinput hook to the selector it is given,
    // so applying it to each attempt is safe; only the selected one stays.
    let select_backend = |software: bool| {
        let mut selector = slint::BackendSelector::new();
        if kms {
            selector = selector.backend_name("linuxkms".into());
        }
        if software {
            selector = selector.renderer_name("software".into());
        } else {
            #[cfg(feature = "wgpu")]
            {
                selector = selector.require_wgpu_30(slint::wgpu_30::WGPUConfiguration::default());
            }
        }
        {
            selector = tty::install(selector);
        }
        selector.select()
    };
    if use_cpu {
        select_backend(true)?;
    } else if let Err(error) = select_backend(false) {
        // Login consoles must come up even without a working GPU stack.
        log::warn!("GPU rendering unavailable, falling back to software: {error}");
        select_backend(true)?;
        use_gpu = false;
    }

    let ui = AppWindow::new()?;
    let terminal = slint_terminal::attach(
        &ui.global::<TerminalApi>(),
        ui.window(),
        slint_terminal::Config {
            command,
            gpu: use_gpu,
            font_px: std::env::var("SLINTCON_FONT_PX")
                .ok()
                .and_then(|value| value.parse().ok()),
            fit_columns: kms.then_some(80),
            ..slint_terminal::Config::default()
        },
    )?;
    ui.global::<TerminalApi>().on_exited(|| {
        let _ = slint::quit_event_loop();
    });
    let terminal = Rc::new(terminal);

    // Slint's KMS presenter drops frames posted in its first moments, so
    // repaint everything a few times after start-up, then stop.
    let burst = Rc::new(slint::Timer::default());
    {
        let terminal = Rc::clone(&terminal);
        let burst_weak = Rc::downgrade(&burst);
        let mut left = STARTUP_REPAINTS;
        burst.start(
            slint::TimerMode::Repeated,
            STARTUP_REPAINT_INTERVAL,
            move || {
                terminal.request_full_repaint();
                left -= 1;
                if left == 0
                    && let Some(burst) = burst_weak.upgrade()
                {
                    burst.stop();
                }
            },
        );
    }

    // The terminal ticks itself; this only polls what slintcon owns: VT
    // switches and keys read from the VT. Fast while a VT is polled.
    let timer = Rc::new(slint::Timer::default());
    {
        let vt = Rc::clone(&vt);
        let terminal = Rc::clone(&terminal);
        let window = ui.as_weak();
        let timer_weak = Rc::downgrade(&timer);
        timer.start(slint::TimerMode::Repeated, POLL_INTERVAL, move || {
            if vt.borrow().wants_exit() {
                // Engaged on the wrong VT: leave so the supervisor
                // restarts us, which waits for the right VT again.
                let _ = slint::quit_event_loop();
            }
            if let Some(change) = vt.borrow_mut().poll() {
                let active = change == tty::VtChange::Resume;
                terminal.set_display_active(active);
                if let (true, Some(ui)) = (active, window.upgrade()) {
                    // The other VT drew over the screen.
                    ui.window().request_redraw();
                }
            }
            // Keys read from the VT (tty-style input); empty when the
            // keyboard is read through evdev and Slint instead.
            let keys = vt.borrow_mut().poll_keys();
            for key in keys {
                terminal.handle_key_event(key);
            }
            // Pointer events cannot know the modifiers when Slint does
            // not see the keyboard, so they come from the VT keyboard.
            terminal.set_external_modifiers(vt.borrow().tty_modifiers());

            if let Some(timer) = timer_weak.upgrade() {
                let interval = if vt.borrow().polls() {
                    POLL_INTERVAL
                } else {
                    IDLE_POLL_INTERVAL
                };
                if timer.interval() != interval {
                    timer.set_interval(interval);
                }
            }
        });
    }

    ui.run()?;
    // The timer holds the terminal and the VT; release them here so the
    // drops below run in order.
    drop(timer);
    drop(burst);
    // Restores the VT mode we own (KMS); Slint already restored its part
    // when the event loop returned. Also runs on the error/unwind paths.
    drop(vt);
    Ok(())
}
