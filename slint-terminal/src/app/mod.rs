//! Terminal session: PTY, rio-vt state, input encoding, and the redraw loop.

use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::sync::FairMutex;
use rio_vt::event::{Msg, WindowSize};

use crate::Config;
use crate::clipboard::Clipboard;
use crate::images::OverlayQuad;
use crate::key::{Key, Modifiers};
use crate::mouse::Protocol;
use crate::renderer::Renderer;
use crate::ui::TerminalApi;
use crate::widget::Wake;

mod frame;
mod keyboard;
mod pointer;
mod state;
mod terminal;

use self::pointer::*;
use self::state::*;
use self::terminal::*;

pub(crate) type Term = Crosswords<TermListener>;

pub struct TerminalApp {
    /// The Slint global the UI state is pushed to.
    api: slint::Weak<TerminalApi<'static>>,
    /// Wakes the UI from the pty and host events.
    wake: Wake,
    /// The terminal state, shared with the pty reader thread.
    term: Arc<FairMutex<Term>>,
    /// Sends input and resizes to the pty.
    pty: rio_vt::corcovado::channel::Sender<Msg>,
    /// Events from the terminal (title, clipboard, exit, graphics).
    events: Receiver<HostEvent>,
    /// Draws rows and overlays into the frame image.
    renderer: Renderer,
    /// What the last frame was painted from.
    cache: FrameCache,
    /// The font size.
    font: FontSize,
    /// When the UI should refresh.
    pacing: Pacing,
    /// The widget's size as last reported; `sync_size` applies it.
    viewport: Viewport,
    /// The program ended; the widget tells the application once.
    exited: bool,
    /// Graphics updates received but not yet handed to the renderer.
    pending_graphics: Option<UpdateQueues>,
    /// False while the host cannot show the display (e.g. another VT owns
    /// it): the terminal keeps consuming PTY output but draws nothing.
    display_active: bool,
    /// Keys held down that triggered a host shortcut (their repeats and
    /// releases are not forwarded).
    shortcut_keys: Vec<Key>,
    /// Repaint every row on the next render (after getting the display back).
    full_redraw: bool,
    /// Clipboard and primary selection.
    clipboard: Clipboard,
    /// Pointer position, selection and autoscroll state.
    pointer: PointerState,
    /// Modifiers supplied by the host when Slint cannot see the keyboard and
    /// so cannot tell pointer events about them.
    external_modifiers: Option<Modifiers>,
    /// Whether the UI was last told a program is using the mouse.
    mouse_reporting_shown: bool,
    /// What the program asked of the mouse, as of the last tick.
    protocol: Protocol,
}

impl TerminalApp {
    /// `initial` is the viewport (physical width and height, scale factor) to
    /// start with; the widget reports the real one once it is laid out.
    pub fn new(
        api: slint::Weak<TerminalApi<'static>>,
        wake: Wake,
        config: &Config,
        initial: (u32, u32, f32),
    ) -> Result<Self, Box<dyn Error>> {
        let (width, height, scale) = initial;
        let use_gpu = config.gpu;
        let mut font = FontSize::new(config);
        let mut renderer = Renderer::new(use_gpu, font.px)?;
        font.refit(renderer.zoom(), width, scale);
        renderer.resize(width, height, font.px);
        let (cols, rows) = grid_size(&renderer, width, height);
        let (terminal, pty_tx, event_rx) =
            spawn_terminal(&renderer, cols, rows, width, height, config, wake.clone())?;
        renderer.set_kitty_source(terminal.clone());

        Ok(Self {
            api,
            wake,
            exited: false,
            term: terminal,
            pty: pty_tx,
            events: event_rx,
            renderer,
            cache: FrameCache::new(cols),
            viewport: Viewport {
                width,
                height,
                scale,
                stale: false,
            },
            pending_graphics: None,
            display_active: true,
            shortcut_keys: Vec::new(),
            full_redraw: false,
            font,
            pacing: Pacing::new(config),
            clipboard: Clipboard::default(),
            pointer: PointerState::default(),
            external_modifiers: None,
            mouse_reporting_shown: false,
            protocol: Protocol::default(),
        })
    }

    /// Posts pending sixel/iTerm2/kitty texture data to the renderer. Called by
    /// the rendering notifier once Slint's wgpu device exists.
    #[cfg(feature = "wgpu")]
    pub fn init_gpu(&mut self, device: wgpu::Device, queue: wgpu::Queue) {
        self.renderer.init_gpu(device, queue);
        // Frames drawn before the device existed had no image to show.
        self.full_redraw = true;
        self.wake.wake();
    }

    /// Repaint every row on the next tick.
    pub fn request_full_repaint(&mut self) {
        self.full_redraw = true;
        self.wake.wake();
    }

    /// Stop or resume drawing, e.g. when the host loses the display. While inactive, `tick`
    /// still drains the PTY and host events so the child never stalls; only
    /// rendering (and anything else that would make Slint present) is skipped.
    pub fn set_display_active(&mut self, active: bool) {
        if self.display_active == active {
            return;
        }
        self.display_active = active;
        if active {
            // The other VT drew over the screen: repaint all of it, even if
            // the terminal itself did not change.
            self.full_redraw = true;
            self.render();
        }
    }

    /// How often the UI should refresh at `now`.
    pub fn refresh_interval(&self, now: Instant) -> Duration {
        self.pacing.interval(now)
    }

    /// Record user input and make sure the UI draws it soon: input that
    /// changes no terminal damage (selection) is only painted by a tick.
    pub(super) fn note_input(&mut self) {
        self.pacing.touch(Instant::now());
        self.wake.wake();
    }

    /// Ask for the widget's size (physical pixels, scale factor); `sync_size`
    /// applies it on the next tick.
    pub fn request_viewport(&mut self, width: u32, height: u32, scale: f32) {
        if width == 0 || height == 0 {
            return;
        }
        let v = &mut self.viewport;
        if (v.width, v.height, v.scale) != (width, height, scale) {
            *v = Viewport {
                width,
                height,
                scale,
                stale: true,
            };
        }
        self.wake.wake();
    }

    /// True once after the program in the terminal ended.
    pub fn take_exited(&mut self) -> bool {
        std::mem::take(&mut self.exited)
    }

    pub fn tick(&mut self) {
        self.sync_size();
        {
            let mut term = self.term.lock();
            term.send_graphics_updates();
            // Read here rather than per pointer event, which can arrive
            // hundreds of times a second and would contend with the pty.
            self.protocol = Protocol::from_mode(term.mode());
        }
        let reporting = self.protocol.active();
        if reporting != self.mouse_reporting_shown
            && let Some(api) = self.api.upgrade()
        {
            self.mouse_reporting_shown = reporting;
            api.set_mouse_reporting(reporting);
        }
        self.autoscroll();

        while let Ok(event) = self.events.try_recv() {
            match event {
                HostEvent::PtyWrite(text) => self.write_input(text.into_bytes()),
                HostEvent::Title(title) => self.set_title(title),
                HostEvent::Close => {
                    let _ = self.pty.send(Msg::Shutdown);
                    self.exited = true;
                    return;
                }
                HostEvent::Clipboard(kind, text) => self.clipboard.set(kind, text),
                HostEvent::Graphics(queues) => match self.pending_graphics.as_mut() {
                    Some(pending) => {
                        pending.pending.extend(queues.pending);
                        pending.pending_images.extend(queues.pending_images);
                        pending.remove_queue.extend(queues.remove_queue);
                    }
                    None => self.pending_graphics = Some(queues),
                },
            }
        }

        self.render();
    }

    /// Apply a viewport/font size to the renderer, terminal, and pty.
    fn apply_size(&mut self) {
        let Viewport { width, height, .. } = self.viewport;
        self.viewport.stale = false;
        self.renderer.resize(width, height, self.font.px);
        self.publish_cell_size();
        let (cols, rows) = grid_size(&self.renderer, width, height);
        self.cache.grid_cols = cols;

        let mut term = self.term.lock();
        term.resize(CrosswordsSize::new_with_dimensions(
            cols,
            rows,
            width,
            height,
            self.renderer.cell_width(),
            self.renderer.cell_height(),
        ));
        drop(term);

        let _ = self.pty.send(Msg::Resize(WindowSize {
            rows: rows.min(u16::MAX as usize) as u16,
            cols: cols.min(u16::MAX as usize) as u16,
            width: width.min(u16::MAX as u32) as u16,
            height: height.min(u16::MAX as u32) as u16,
        }));
        for row in &mut self.cache.snapshot.rows {
            row.dirty = true;
        }
    }

    /// The widget's `font-size` changed; `px` is in physical pixels, 0 for
    /// automatic.
    pub fn request_font(&mut self, px: u32) {
        self.font.request(px);
        self.font.refit(
            self.renderer.zoom(),
            self.viewport.width,
            self.viewport.scale,
        );
        self.apply_size();
    }

    /// Tell the widget the cell size so it can derive its preferred size.
    pub fn publish_cell_size(&self) {
        if let Some(api) = self.api.upgrade() {
            api.set_cell_width(self.renderer.cell_width() as f32);
            api.set_cell_height(self.renderer.cell_height() as f32);
        }
    }

    /// Set an explicit font size (stops following the automatic default).
    fn set_font_px(&mut self, font_px: u32) {
        if self.font.set(font_px) {
            self.apply_size();
        }
    }

    fn sync_size(&mut self) {
        if !self.viewport.stale {
            return;
        }
        self.font.refit(
            self.renderer.zoom(),
            self.viewport.width,
            self.viewport.scale,
        );
        self.apply_size();
    }
}

impl Drop for TerminalApp {
    fn drop(&mut self) {
        let _ = self.pty.send(Msg::Shutdown);
    }
}
