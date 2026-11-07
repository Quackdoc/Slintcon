//! Renderer selection: the CPU and GPU renderers share one narrow interface.

use std::error::Error;
use std::sync::Arc;

use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::config::colors::term::TermColors;
use rio_vt::event::sync::FairMutex;

use crate::app::Term;
use crate::cpu_renderer::CpuRenderer;
#[cfg(feature = "wgpu")]
use crate::gpu_renderer::GpuRenderer;
use crate::images::OverlayQuad;
use crate::term_style::{RowSource, RowView, ZoomLevels};

pub enum Renderer {
    Cpu(Box<CpuRenderer>),
    #[cfg(feature = "wgpu")]
    Gpu(Box<GpuRenderer>),
}

/// Run `$body` with `$r` bound to whichever renderer is active.
macro_rules! each {
    ($self:expr, $r:ident => $body:expr) => {
        match $self {
            Renderer::Cpu($r) => $body,
            #[cfg(feature = "wgpu")]
            Renderer::Gpu($r) => $body,
        }
    };
}

impl Renderer {
    pub fn new(use_gpu: bool, font_px: u32) -> Result<Self, Box<dyn Error>> {
        #[cfg(feature = "wgpu")]
        if use_gpu {
            return Ok(Renderer::Gpu(Box::new(GpuRenderer::new(font_px)?)));
        }
        #[cfg(not(feature = "wgpu"))]
        let _ = use_gpu;
        Ok(Renderer::Cpu(Box::new(CpuRenderer::new(font_px)?)))
    }

    pub fn cell_width(&self) -> u32 {
        each!(self, r => r.cell_width())
    }

    pub fn cell_height(&self) -> u32 {
        each!(self, r => r.cell_height())
    }

    /// Font sizes Ctrl+/Ctrl- step through (see [`ZoomLevels`]).
    pub fn zoom(&self) -> &ZoomLevels {
        each!(self, r => r.zoom())
    }

    pub fn resize(&mut self, width: u32, height: u32, font_px: u32) {
        each!(self, r => r.resize(width, height, font_px))
    }

    /// Give the renderer read access to the terminal's image store, so it
    /// need not keep its own copy of kitty pixels (CPU renderer only).
    pub fn set_kitty_source(&mut self, source: Arc<FairMutex<Term>>) {
        match self {
            Renderer::Cpu(r) => r.set_kitty_source(source),
            // The GPU renderer uploads kitty pixels as textures instead.
            #[cfg(feature = "wgpu")]
            Renderer::Gpu(_) => {}
        }
    }

    /// Take the image updates rio-vt queued.
    pub fn apply_images(&mut self, queues: UpdateQueues) {
        each!(self, r => r.apply_images(queues))
    }

    pub fn set_overlays(&mut self, overlays: Vec<OverlayQuad>) {
        each!(self, r => r.set_overlays(overlays))
    }

    pub fn draw_row(
        &mut self,
        source: RowSource,
        line: usize,
        view: RowView,
        term_colors: &TermColors,
    ) {
        each!(self, r => r.draw_row(source, line, view, term_colors))
    }

    pub fn end_frame(&mut self) {
        each!(self, r => r.end_frame())
    }

    pub fn image(&mut self) -> Option<slint::Image> {
        each!(self, r => r.image())
    }

    #[cfg(feature = "wgpu")]
    pub fn init_gpu(&mut self, device: wgpu::Device, queue: wgpu::Queue) {
        if let Renderer::Gpu(r) = self {
            r.init_gpu(device, queue);
        }
    }
}
