//! Connecting the terminal to a Slint window: callbacks, timer and wake-ups.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::app::TerminalApp;
use crate::key::{self, KeyEvent, Modifiers};
use crate::mouse;
use crate::ui::TerminalApi;
use slint::Global;

/// How to start the terminal.
#[derive(Clone, Debug)]
pub struct Config {
    /// Program and arguments to run; empty means the user's shell.
    pub command: Vec<String>,
    /// Render on the GPU. Needs the `wgpu` feature and a Slint wgpu renderer.
    pub gpu: bool,
    /// Fixed font size in physical pixels; `None` picks one for the display.
    pub font_px: Option<u32>,
    /// Pick the largest font that still fits this many columns.
    pub fit_columns: Option<u32>,
    /// Lines of scrollback history kept.
    pub scrollback: usize,
    /// Refresh interval while the terminal is busy; sets the highest frame rate.
    pub frame_interval: Duration,
    /// Refresh interval once idle: output and input wake the UI on their own,
    /// so this only has to cover resizes and mouse mode changes.
    pub idle_interval: Duration,
    /// How long after output or input the UI keeps `frame_interval` before
    /// backing off to `idle_interval`.
    pub idle_delay: Duration,
}

#[allow(clippy::derivable_impls)] // `gpu` depends on the feature
impl Default for Config {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            gpu: cfg!(feature = "wgpu"),
            font_px: None,
            fit_columns: None,
            scrollback: 10_000,
            frame_interval: Duration::from_millis(16),
            idle_interval: Duration::from_millis(100),
            idle_delay: Duration::from_millis(250),
        }
    }
}

type Tick = Rc<dyn Fn()>;

thread_local! {
    /// Tick closures by terminal id; only touched on the UI thread.
    static TICKS: RefCell<HashMap<u64, Tick>> = RefCell::new(HashMap::new());
}

/// Asks the UI thread to tick one terminal; callable from any thread.
#[derive(Clone)]
pub(crate) struct Wake {
    id: u64,
    pending: Arc<AtomicBool>,
}

impl Wake {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            pending: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Ticks queued while one is pending are coalesced into it.
    pub(crate) fn wake(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let (id, pending) = (self.id, Arc::clone(&self.pending));
        let queued = slint::invoke_from_event_loop(move || {
            pending.store(false, Ordering::Release);
            // Clone out so the tick can wake again safely.
            let tick = TICKS.with(|ticks| ticks.borrow().get(&id).cloned());
            if let Some(tick) = tick {
                tick();
            }
        });
        if queued.is_err() {
            // No event loop (yet, or any more): the timer still ticks.
            self.pending.store(false, Ordering::Release);
        }
    }
}

/// A running terminal. Dropping it stops the terminal and its program.
pub struct Terminal {
    app: Rc<RefCell<TerminalApp>>,
    timer: Rc<slint::Timer>,
    id: u64,
}

impl Terminal {
    /// Stop or resume drawing, e.g. when the display is lost; the program keeps running.
    pub fn set_display_active(&self, active: bool) {
        self.app.borrow_mut().set_display_active(active);
    }

    /// Repaint every row on the next tick, e.g. when the presenter may have
    /// dropped frames.
    pub fn request_full_repaint(&self) {
        self.app.borrow_mut().request_full_repaint();
    }

    /// Send a key that did not come through Slint.
    pub fn handle_key_event(&self, event: KeyEvent) {
        self.app.borrow_mut().handle_key_event(event);
    }

    /// Modifier state for pointer events when Slint cannot see the keyboard.
    pub fn set_external_modifiers(&self, mods: Option<Modifiers>) {
        self.app.borrow_mut().set_external_modifiers(mods);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.timer.stop();
        TICKS.with(|ticks| ticks.borrow_mut().remove(&self.id));
    }
}

fn modifiers_from(shift: bool, ctrl: bool, alt: bool) -> Modifiers {
    let mut mods = Modifiers::empty();
    mods.set(key::Modifiers::SHIFT, shift);
    mods.set(key::Modifiers::CTRL, ctrl);
    mods.set(key::Modifiers::ALT, alt);
    mods
}

/// Start a terminal in the `Terminal` widget of `window`.
///
/// Call it once the window exists and before it runs. The program ending
/// invokes the `exited` callback of `api`.
pub fn attach(
    api: &TerminalApi<'_>,
    window: &slint::Window,
    config: Config,
) -> Result<Terminal, Box<dyn Error>> {
    let size = window.size();
    let initial = (size.width.max(1), size.height.max(1), window.scale_factor());
    let wake = Wake::new();
    let id = wake.id;
    let app = Rc::new(RefCell::new(TerminalApp::new(
        api.as_weak(),
        wake,
        &config,
        initial,
    )?));

    #[cfg(feature = "wgpu")]
    if config.gpu {
        let app = Rc::downgrade(&app);
        window
            .set_rendering_notifier(move |state, graphics_api| {
                if let slint::RenderingState::RenderingSetup = state
                    && let slint::GraphicsAPI::WGPU30 { device, queue, .. } = graphics_api
                    && let Some(app) = app.upgrade()
                {
                    app.borrow_mut().init_gpu(device.clone(), queue.clone());
                }
            })
            .map_err(|error| format!("cannot observe wgpu rendering setup: {error}"))?;
    }

    // A `font-size` set on the element wins over the configured default.
    app.borrow_mut()
        .request_font(api.get_font_px().round() as u32);
    api.on_font_changed({
        let app = Rc::downgrade(&app);
        let api = api.as_weak();
        move || {
            if let (Some(app), Some(api)) = (app.upgrade(), api.upgrade()) {
                app.borrow_mut()
                    .request_font(api.get_font_px().round() as u32);
            }
        }
    });
    app.borrow().publish_cell_size();

    api.on_key({
        let app = Rc::downgrade(&app);
        move |event| match app.upgrade() {
            Some(app) => app.borrow_mut().handle_key(&event),
            None => slint::private_unstable_api::re_exports::EventResult::Reject,
        }
    });
    api.on_scroll({
        let app = Rc::downgrade(&app);
        move |delta_y, x, y, shift, ctrl, alt| {
            if let Some(app) = app.upgrade() {
                let mods = modifiers_from(shift, ctrl, alt);
                app.borrow_mut().handle_scroll(delta_y, x, y, mods);
            }
        }
    });
    api.on_pointer({
        let app = Rc::downgrade(&app);
        move |kind, button, x, y, shift, ctrl, alt| {
            if let Some(app) = app.upgrade() {
                app.borrow_mut().handle_pointer(
                    mouse::Kind::from_index(kind),
                    mouse::Button::from_index(button),
                    x,
                    y,
                    modifiers_from(shift, ctrl, alt),
                );
            }
        }
    });
    api.on_viewport({
        let app = Rc::downgrade(&app);
        move |width, height, scale| {
            if let Some(app) = app.upgrade() {
                app.borrow_mut().request_viewport(
                    width.round() as u32,
                    height.round() as u32,
                    scale,
                );
            }
        }
    });

    // Output and host events wake the UI through `Wake`. The timer covers
    // what has no event of its own (mouse autoscroll, a changed clock) and
    // backs off while the terminal is idle.
    let timer = Rc::new(slint::Timer::default());
    let tick: Tick = {
        let app = Rc::downgrade(&app);
        let timer = Rc::downgrade(&timer);
        let api = api.as_weak();
        let ticking = Cell::new(false);
        Rc::new(move || {
            let Some(app) = app.upgrade() else {
                return;
            };
            // A tick can re-enter through a callback; skip rather than panic.
            if ticking.replace(true) {
                return;
            }
            let (interval, exited) = {
                let mut app = app.borrow_mut();
                app.tick();
                (app.refresh_interval(Instant::now()), app.take_exited())
            };
            ticking.set(false);
            if exited && let Some(api) = api.upgrade() {
                api.invoke_exited();
            }
            if let Some(timer) = timer.upgrade()
                && timer.interval() != interval
            {
                timer.set_interval(interval);
            }
        })
    };
    TICKS.with(|ticks| ticks.borrow_mut().insert(id, Rc::clone(&tick)));
    {
        let tick = Rc::clone(&tick);
        timer.start(
            slint::TimerMode::Repeated,
            config.frame_interval,
            move || tick(),
        );
    }

    // Draw once now so a fullscreen display shows content before any input.
    tick();

    Ok(Terminal { app, timer, id })
}
