//! A terminal emulator widget for Slint.
//!
//! Import `Terminal` and `TerminalApi` from `"@SlintTerminal"` in your `.slint`
//! file (re-exporting `TerminalApi` from your window), then call [`attach`].
#![deny(unsafe_code)]

mod app;
mod canvas;
mod clipboard;
mod cpu_renderer;
#[cfg(feature = "wgpu")]
mod gpu_renderer;
mod images;
pub mod key;
mod mouse;
mod overlays;
mod renderer;
mod slint_key;
mod term_style;
mod widget;

/// The widget's Slint types, for `window.global::<slint_terminal::ui::TerminalApi>()`.
pub mod ui {
    slint::include_modules!();
}

pub use widget::{Config, Terminal, attach};
