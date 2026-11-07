//! Graphics image handling shared by both renderers: the decoded RGBA cache
//! for the pixels rio-vt hands over once, and the overlay quads.

use std::borrow::Cow;
use std::collections::HashMap;

use rio_graphics::{ColorType, GraphicData, atlas_image_key, kitty_image_key};
use rio_vt::ansi::graphics::UpdateQueues;

/// Owned pixels of an image that rio-vt does not keep (sixel/iTerm2), or
/// that a renderer stages until it can upload them. Pixels are stored as
/// received (RGB or RGBA); nothing is converted or copied.
pub struct ImagePixels {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub data: Vec<u8>,
}

/// Borrowed view of image pixels (RGB or RGBA), either ours or rio-vt's.
#[derive(Clone, Copy)]
pub struct ImageView<'a> {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub data: &'a [u8],
}

impl<'a> ImageView<'a> {
    /// View rio-vt's pixels in place; `None` if empty or malformed.
    pub fn from_graphic(data: &'a GraphicData) -> Option<Self> {
        let channels = match data.color_type {
            ColorType::Rgb => 3,
            ColorType::Rgba => 4,
        };
        let (width, height) = (data.width as u32, data.height as u32);
        if width == 0
            || height == 0
            || data.pixels.len() < width as usize * height as usize * channels as usize
        {
            return None;
        }
        Some(Self {
            width,
            height,
            channels,
            data: &data.pixels,
        })
    }
}

impl ImageView<'_> {
    /// True if no pixel is transparent.
    pub fn is_opaque(&self) -> bool {
        self.channels == 3 || self.data.as_chunks::<4>().0.iter().all(|px| px[3] == 255)
    }
}

impl ImagePixels {
    pub fn view(&self) -> ImageView<'_> {
        ImageView {
            width: self.width,
            height: self.height,
            channels: self.channels,
            data: &self.data,
        }
    }

    /// The pixels as RGBA8, borrowed if already RGBA.
    #[cfg_attr(not(feature = "wgpu"), allow(dead_code))]
    pub fn rgba(&self) -> Cow<'_, [u8]> {
        if self.channels == 4 {
            return Cow::Borrowed(&self.data);
        }
        let mut rgba = Vec::with_capacity(self.data.len() / 3 * 4);
        for pixel in self.data.as_chunks::<3>().0 {
            rgba.extend_from_slice(pixel);
            rgba.push(255);
        }
        Cow::Owned(rgba)
    }
}

/// A renderer-agnostic image placement, resolved to screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayQuad {
    /// Texture key ([`kitty_image_key`] / [`atlas_image_key`]).
    pub image_key: u64,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Normalized source rectangle `[u0, v0, u1, v1]`.
    pub src: [f32; 4],
    /// False for sixel/iTerm2 and negative-kitty z-indexes (drawn under text),
    /// true for kitty placements at z >= 0 and Unicode placeholder runs.
    pub over_text: bool,
    /// Stacking order within `over_text`.
    pub z: i32,
}

/// Sort the quads into stacking order and drop those fully hidden behind a
/// later opaque quad (`opaque` says whether an image has no transparency). A
/// video player that places a new full-screen image every frame leaves dozens
/// of stacked quads in rio-vt's store; drawing them all is what made that slow.
pub fn cull_hidden(overlays: &mut Vec<OverlayQuad>, mut opaque: impl FnMut(u64) -> bool) {
    overlays.sort_by_key(|q| (q.over_text, q.z, q.image_key));
    let mut covers: Vec<[f32; 4]> = Vec::new();
    let mut hidden = vec![false; overlays.len()];
    for (quad, hidden) in overlays.iter().zip(&mut hidden).rev() {
        let rect = [quad.x, quad.y, quad.x + quad.width, quad.y + quad.height];
        if covers
            .iter()
            .any(|c| c[0] <= rect[0] && c[1] <= rect[1] && c[2] >= rect[2] && c[3] >= rect[3])
        {
            *hidden = true;
        } else if opaque(quad.image_key) {
            covers.push(rect);
        }
    }
    let mut hidden = hidden.into_iter();
    overlays.retain(|_| !hidden.next().unwrap_or(false));
}

/// True for keys of atlas graphics (sixel/iTerm2), whose pixels exist only
/// in our cache. Kitty keys (`kitty_image_key`) stay in rio-vt's store.
pub fn is_atlas_key(key: u64) -> bool {
    key >= 1 << 32
}

/// Pixels rio-vt hands over once. Kitty pixels stay in rio-vt's store, which
/// the CPU renderer reads in place; the GPU renderer stages them here until
/// the next upload.
#[derive(Default)]
pub struct ImagePixelsCache {
    images: HashMap<u64, ImagePixels>,
}

impl ImagePixelsCache {
    pub fn get(&self, key: u64) -> Option<&ImagePixels> {
        self.images.get(&key)
    }

    /// Take every staged image (the caller uploads and drops them).
    #[cfg_attr(not(feature = "wgpu"), allow(dead_code))]
    pub fn drain(&mut self) -> impl Iterator<Item = (u64, ImagePixels)> + '_ {
        self.images.drain()
    }

    /// Merge one batch of rio-vt graphics updates, moving the pixels.
    /// Kitty images are only taken when `keep_kitty`. Returns the keys
    /// rio-vt removed, for the caller to drop anything derived from them.
    pub fn apply(&mut self, queues: UpdateQueues, keep_kitty: bool) -> Vec<u64> {
        for data in queues.pending {
            self.insert(atlas_image_key(data.id.get()), data);
        }
        if keep_kitty {
            for (image_id, data) in queues.pending_images {
                self.insert(kitty_image_key(image_id), data);
            }
        }
        for key in &queues.remove_queue {
            self.images.remove(key);
        }
        queues.remove_queue
    }

    fn insert(&mut self, key: u64, data: GraphicData) {
        let Some(view) = ImageView::from_graphic(&data) else {
            return;
        };
        let (width, height, channels) = (view.width, view.height, view.channels);
        self.images.insert(
            key,
            ImagePixels {
                width,
                height,
                channels,
                data: data.pixels,
            },
        );
    }
}
