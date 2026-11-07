//! Spawning the terminal and its pty, and the events the pty thread sends back.

use std::error::Error;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use rio_vt::ansi::CursorShape;
use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::sync::FairMutex;
use rio_vt::event::{EventListener, Msg, RioEvent, WindowId};
use rio_vt::performer::Machine;
use rio_vt::teletypewriter::create_pty_with_spawn;

use crate::Config;
use crate::clipboard;
use crate::renderer::Renderer;

use super::*;

/// Terminal grid size `(cols, rows)` that fits the viewport, at least 1x1.
pub(super) fn grid_size(renderer: &Renderer, width: u32, height: u32) -> (usize, usize) {
    (
        (width / renderer.cell_width().max(1)).max(1) as usize,
        (height as f32 / renderer.cell_height() as f32)
            .floor()
            .max(1.0) as usize,
    )
}

/// Terminal state, pty sender, and host-event receiver.
pub(super) type SpawnedTerminal = (
    Arc<FairMutex<Term>>,
    rio_vt::corcovado::channel::Sender<Msg>,
    Receiver<HostEvent>,
);

/// Create the terminal state, spawn the pty and its reader thread.
pub(super) fn spawn_terminal(
    renderer: &Renderer,
    cols: usize,
    rows: usize,
    width: u32,
    height: u32,
    config: &Config,
    wake: Wake,
) -> Result<SpawnedTerminal, Box<dyn Error>> {
    let (event_tx, event_rx) = mpsc::channel();
    let listener = TermListener {
        events: event_tx,
        wake,
    };

    let terminal = Arc::new(FairMutex::new(Crosswords::new(
        CrosswordsSize::new_with_dimensions(
            cols,
            rows,
            width,
            height,
            renderer.cell_width(),
            renderer.cell_height(),
        ),
        CursorShape::Block,
        listener.clone(),
        WindowId::from(1),
        0,
        config.scrollback,
    )));

    let env = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
        ("TERM_PROGRAM".to_string(), "slintcon".to_string()),
    ];

    // Without a command the user's login shell runs.
    let (program, arguments) = match config.command.split_first() {
        Some((program, arguments)) => (Some(program.as_str()), arguments.to_vec()),
        None => (None, Vec::new()),
    };
    let pty = create_pty_with_spawn(
        program,
        arguments,
        &None,
        Some(env),
        cols.min(u16::MAX as usize) as u16,
        rows.min(u16::MAX as usize) as u16,
        width.min(u16::MAX as u32) as u16,
        height.min(u16::MAX as u32) as u16,
    )?;

    let machine = Machine::new(Arc::clone(&terminal), pty, listener, WindowId::from(1), 0)?;
    let pty_tx = machine.channel();
    // The reader thread owns the machine from here on. Dropping the join
    // handle detaches it, which is what a long-lived terminal wants.
    drop(machine.spawn());
    Ok((terminal, pty_tx, event_rx))
}

#[derive(Clone)]
pub(crate) struct TermListener {
    events: Sender<HostEvent>,
    wake: Wake,
}

pub(super) enum HostEvent {
    PtyWrite(String),
    Title(String),
    Close,
    /// A batch of sixel/iTerm2/kitty texture uploads or removals.
    Graphics(UpdateQueues),
    /// OSC 52: the program sets the clipboard.
    Clipboard(clipboard::Kind, String),
}

impl TermListener {
    /// Queue a host event for the next UI tick and wake the UI thread for it.
    fn post(&self, event: HostEvent) {
        let _ = self.events.send(event);
        self.wake.wake();
    }
}

impl EventListener for TermListener {
    fn send_event(&self, event: RioEvent, _window_id: WindowId) {
        match event {
            // The pty thread has new damage for the renderer; it is read
            // from the terminal state on the next tick.
            RioEvent::TerminalDamaged(_) => self.wake.wake(),
            // Responses to terminal queries (DA, DSR, ...) have to go back to
            // the program, otherwise shells and full-screen apps hang waiting
            // for them.
            RioEvent::PtyWrite(_, text) => {
                self.post(HostEvent::PtyWrite(text));
            }
            RioEvent::Title(_, title) => {
                self.post(HostEvent::Title(title));
            }
            RioEvent::ChildExited(_, status) => {
                log::info!("the shell exited: {}", describe_wait_status(status));
            }
            RioEvent::CloseTerminal(_) | RioEvent::Exit => {
                log::info!("the shell ended; closing the terminal");
                self.post(HostEvent::Close);
            }
            RioEvent::UpdateGraphics { queues, .. } => {
                self.post(HostEvent::Graphics(queues));
            }
            RioEvent::ClipboardStore(kind, text) => {
                let kind = match kind {
                    rio_vt::clipboard::ClipboardType::Clipboard => clipboard::Kind::Clipboard,
                    rio_vt::clipboard::ClipboardType::Selection => clipboard::Kind::Primary,
                };
                self.post(HostEvent::Clipboard(kind, text));
            }
            _ => {}
        }
    }
}

/// A raw `waitpid` status, in words.
pub(super) fn describe_wait_status(status: Option<i32>) -> String {
    let Some(raw) = status else {
        return "status unknown".to_string();
    };
    use std::os::unix::process::ExitStatusExt;
    let status = std::process::ExitStatus::from_raw(raw);
    match (status.code(), status.signal()) {
        (Some(code), _) => return format!("exit code {code}"),
        (None, Some(signal)) => return format!("killed by signal {signal}"),
        _ => {}
    }
    format!("raw status {raw:#x}")
}
