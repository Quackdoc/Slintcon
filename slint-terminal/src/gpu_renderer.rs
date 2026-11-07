//! GPU renderer.
//!
//! The terminal is drawn into an offscreen texture that Slint samples. One wgpu
//! pass per damaged frame draws quads, images below text, glyphon text,
//! decorations and cursor, then images above text. Text shaping is
//! cosmic-text's, as in the CPU renderer; glyphon only rasterizes it.

use std::collections::HashMap;

use cosmic_text::Buffer;
use glyphon::{ColorMode, Resolution, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport};
use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::config::colors::term::{List, TermColors};
use wgpu::util::DeviceExt;

use crate::images::{ImagePixels, ImagePixelsCache, OverlayQuad, cull_hidden};
use crate::term_style::{
    self, ColorScheme, FontEnv, PRect, RowPaint, RowSource, RowView, ZoomLevels,
};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r#"
struct Instance {
    @location(1) pos: vec4<f32>,
    @location(2) data: vec4<f32>,
};

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) data: vec4<f32>,
};

fn corner(vid: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    return corners[vid];
}

// Solid quad: `data` is the color.
@vertex
fn vs_solid(@builtin(vertex_index) vid: u32, i: Instance) -> Out {
    let c = corner(vid);
    return Out(vec4<f32>(mix(i.pos.xy, i.pos.zw, c), 0.0, 1.0), i.data);
}

@fragment
fn fs_solid(in: Out) -> @location(0) vec4<f32> {
    return in.data;
}

// Image quad: `data` is the source rectangle (u0, v0, u1, v1).
@group(0) @binding(0) var image_tex: texture_2d<f32>;

@vertex
fn vs_image(@builtin(vertex_index) vid: u32, i: Instance) -> Out {
    let c = corner(vid);
    return Out(
        vec4<f32>(mix(i.pos.xy, i.pos.zw, c), 0.0, 1.0),
        vec4<f32>(mix(i.data.xy, i.data.zw, c), 0.0, 0.0),
    );
}

// Nearest-neighbour: images are scaled to the cell grid, never smoothed.
@fragment
fn fs_image(in: Out) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(image_tex));
    let texel = min(vec2<i32>(in.data.xy * vec2<f32>(size)), size - vec2<i32>(1, 1));
    return textureLoad(image_tex, texel, 0);
}
"#;

/// One quad. `data` is the color for solids, the source rectangle for images.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    /// Corners `[left, top, right, bottom]` in normalized device coordinates.
    pos: [f32; 4],
    /// Color (solids) or normalized source rectangle (images).
    data: [f32; 4],
}

impl Instance {
    /// A solid quad for `rect` on a target of `size`, or `None` if it is empty.
    fn solid(size: (u32, u32), rect: &PRect) -> Option<Self> {
        (rect.width > 0.0 && rect.height > 0.0).then(|| Self {
            pos: ndc_rect(size, (rect.x, rect.y), (rect.width, rect.height)),
            data: rect.color.map(|byte| f32::from(byte) / 255.0),
        })
    }

    /// A textured quad for `quad` on a target of `size`.
    fn image(size: (u32, u32), quad: &OverlayQuad) -> Self {
        Self {
            pos: ndc_rect(size, (quad.x, quad.y), (quad.width, quad.height)),
            data: quad.src,
        }
    }
}

/// A run of instances in the shared instance buffer drawn with one texture.
struct ImageBatch {
    /// Drawn above the text rather than below.
    over_text: bool,
    /// Image texture key.
    key: u64,
    /// First instance.
    start: u32,
    /// Number of instances.
    count: u32,
}

/// What one frame draws, in layering order.
struct Frame<'a> {
    /// Color the target is cleared to.
    clear: wgpu::Color,
    /// Instances of the background solids, drawn first.
    backgrounds: std::ops::Range<u32>,
    /// Instances of the decorations and cursor bars, drawn over the text.
    foregrounds: std::ops::Range<u32>,
    /// Image draws, below and above the text.
    batches: &'a [ImageBatch],
}

/// An uploaded image; `opaque` lets fully covered quads below it be skipped.
struct GpuImage {
    /// The texture bound for drawing.
    bind_group: wgpu::BindGroup,
    /// Whether no pixel is transparent.
    opaque: bool,
}

/// The wgpu device, render target, pipelines and textures; created once Slint hands over the device.
struct Gpu {
    /// The device shared with Slint.
    device: wgpu::Device,
    /// The queue shared with Slint.
    queue: wgpu::Queue,
    /// The offscreen render target Slint samples.
    texture: wgpu::Texture,
    /// A view of `texture`.
    view: wgpu::TextureView,
    /// Pipeline for solid-colored quads.
    solid_pipeline: wgpu::RenderPipeline,
    /// Pipeline for textured quads.
    image_pipeline: wgpu::RenderPipeline,
    /// All quads of a frame: backgrounds, foreground solids, then images.
    instances: wgpu::Buffer,
    /// Number of instances `instances` has room for.
    capacity: u64,
    /// Uploaded images by key.
    textures: HashMap<u64, GpuImage>,
    /// glyphon's glyph atlas.
    atlas: TextAtlas,
    /// glyphon's viewport (the target size).
    viewport: Viewport,
    /// glyphon's text renderer.
    text_renderer: TextRenderer,
}

/// Terminal renderer drawing through wgpu into a texture Slint samples.
pub struct GpuRenderer {
    /// Font system and cell metrics.
    env: FontEnv,
    /// Glyph rasterizer used by glyphon.
    swash: cosmic_text::SwashCache,
    /// The default color palette.
    palette: List,
    /// Target width in pixels (at least 1).
    width: u32,
    /// Target height in pixels (at least 1).
    height: u32,
    /// Per row, the independently shaped text segments and their x origin.
    text_rows: Vec<Vec<(f32, Buffer)>>,
    /// Per row, what was last painted (rectangles and runs).
    rows: Vec<Option<RowPaint>>,
    /// Images received but not yet uploaded; emptied on every frame, so no
    /// CPU-side pixels are retained once a texture exists.
    staged: ImagePixelsCache,
    /// Image quads in stacking order.
    overlays: Vec<OverlayQuad>,
    /// Per-frame scratch, kept to reuse the allocations.
    quads: Vec<Instance>,
    /// Per-frame image draws, in `quads` order.
    batches: Vec<ImageBatch>,
    /// The GPU state, `None` until [`GpuRenderer::init_gpu`].
    gpu: Option<Gpu>,
}

impl GpuRenderer {
    /// A renderer at `font_px`; it draws nothing until the GPU is attached.
    pub fn new(font_px: u32) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            env: FontEnv::new(font_px),
            swash: cosmic_text::SwashCache::new(),
            palette: term_style::default_palette(),
            width: 1,
            height: 1,
            text_rows: Vec::new(),
            rows: Vec::new(),
            staged: ImagePixelsCache::default(),
            overlays: Vec::new(),
            quads: Vec::new(),
            batches: Vec::new(),
            gpu: None,
        })
    }

    /// Cell width in pixels.
    pub fn cell_width(&self) -> u32 {
        self.env.metrics.cell_width
    }

    /// Cell height in pixels.
    pub fn cell_height(&self) -> u32 {
        self.env.metrics.cell_height
    }

    /// The zoom levels for the font.
    pub fn zoom(&self) -> &ZoomLevels {
        self.env.zoom()
    }

    /// Resize the target and remeasure the font; every row repaints.
    pub fn resize(&mut self, width: u32, height: u32, font_px: u32) {
        self.env.remeasure(font_px);
        self.width = width.max(1);
        self.height = height.max(1);
        self.text_rows.clear();
        self.rows.clear();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.resize(self.width, self.height);
        }
    }

    /// Attach the device/queue handed to us by Slint's rendering notifier.
    pub fn init_gpu(&mut self, device: wgpu::Device, queue: wgpu::Queue) {
        if self.gpu.is_some() {
            return;
        }
        self.gpu = Some(Gpu::new(device, queue, self.width, self.height));
    }

    /// Stage all images (sixel, iTerm2, kitty) for upload, dropping the textures
    /// of removed ones.
    pub fn apply_images(&mut self, queues: UpdateQueues) {
        let removed = self.staged.apply(queues, true);
        if let Some(gpu) = self.gpu.as_mut() {
            for key in removed {
                gpu.textures.remove(&key);
            }
        }
    }

    /// Set the image quads, in stacking order.
    pub fn set_overlays(&mut self, overlays: Vec<OverlayQuad>) {
        self.overlays = overlays;
    }

    /// Paint one row. `line` is viewport-relative.
    pub fn draw_row(
        &mut self,
        source: RowSource,
        line: usize,
        view: RowView,
        term_colors: &TermColors,
    ) {
        let Some(cell) = self.env.cell_size() else {
            return;
        };
        let row_y = line as f32 * cell.height;

        if self.rows.len() <= line {
            self.rows.resize_with(line + 1, || None);
        }
        let paint = self.rows[line].get_or_insert_with(RowPaint::default);
        let colors = ColorScheme {
            term: term_colors,
            palette: &self.palette,
        };
        term_style::paint_row(source, view, colors, self.width, cell, row_y, paint);

        // Keep the shaped row text in sync.
        if self.text_rows.len() <= line {
            self.text_rows.resize_with(line + 1, Vec::new);
        }
        let segments = term_style::text_segments(&paint.runs);
        let row_text = &mut self.text_rows[line];
        row_text.truncate(segments.len());
        while row_text.len() < segments.len() {
            row_text.push((0.0, self.env.make_buffer()));
        }
        for ((left, buffer), segment) in row_text.iter_mut().zip(segments) {
            *left = segment[0].col as f32 * cell.width;
            self.env.set_row_text(buffer, segment);
        }
    }

    /// Housekeeping after a painted frame.
    pub fn end_frame(&mut self) {
        self.env.end_frame();
    }

    /// Render the current frame and hand the texture to Slint.
    pub fn image(&mut self) -> Option<slint::Image> {
        self.gpu.as_ref()?;

        let font_system = &mut self.env.font_system;
        self.text_rows
            .iter_mut()
            .flatten()
            .for_each(|(_, buffer)| buffer.shape_until_scroll(font_system, false));

        let cell_height = self.env.metrics.cell_height as f32;
        let gpu = self.gpu.as_mut()?;

        // Upload new images and free their pixels. An incoming key always
        // replaces the texture we hold; removals come from `apply_images`.
        for (key, img) in self.staged.drain() {
            let image = gpu.upload_image(&img);
            gpu.textures.insert(key, image);
        }

        let size = gpu.size();
        let rows = self.rows.iter().flatten();
        let quads = &mut self.quads;
        quads.clear();
        quads.extend(
            rows.clone()
                .flat_map(|row| &row.backgrounds)
                .filter_map(|rect| Instance::solid(size, rect)),
        );
        let bg_end = quads.len() as u32;
        quads.extend(
            rows.flat_map(|row| row.decorations.iter().chain(&row.cursor_bars))
                .filter_map(|rect| Instance::solid(size, rect)),
        );
        let fg_end = quads.len() as u32;

        // Image overlays in stacking order (below text first), minus those
        // hidden by an opaque one; entries sharing a texture become one
        // instanced draw.
        let batches = &mut self.batches;
        batches.clear();
        cull_hidden(&mut self.overlays, |key| {
            gpu.textures.get(&key).is_some_and(|image| image.opaque)
        });
        let ordered = self
            .overlays
            .iter()
            .filter(|q| q.width > 0.0 && q.height > 0.0);
        for quad in ordered {
            match batches.last_mut() {
                Some(b) if b.key == quad.image_key && b.over_text == quad.over_text => {
                    b.count += 1;
                }
                _ => batches.push(ImageBatch {
                    over_text: quad.over_text,
                    key: quad.image_key,
                    start: quads.len() as u32,
                    count: 1,
                }),
            }
            quads.push(Instance::image(size, quad));
        }

        gpu.write_instances(quads);
        gpu.viewport.update(
            &gpu.queue,
            Resolution {
                width: size.0,
                height: size.1,
            },
        );

        let areas = build_text_areas(&self.text_rows, cell_height, size);
        gpu.text_renderer
            .prepare(
                &gpu.device,
                &gpu.queue,
                &mut self.env.font_system,
                &mut gpu.atlas,
                &gpu.viewport,
                areas,
                &mut self.swash,
            )
            .ok()?;

        let [r, g, b, _] =
            term_style::default_background(&self.palette).map(|channel| f64::from(channel) / 255.0);
        gpu.render(&Frame {
            clear: wgpu::Color { r, g, b, a: 1.0 },
            backgrounds: 0..bg_end,
            foregrounds: bg_end..fg_end,
            batches,
        })
    }
}

impl Gpu {
    /// Draw `frame` into the target, submit it and hand the texture to Slint.
    fn render(&mut self, frame: &Frame) -> Option<slint::Image> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("slintcon-terminal"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("slintcon-terminal-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(frame.clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            self.draw_solids(&mut pass, frame.backgrounds.clone());
            self.draw_images(&mut pass, frame.batches, false);
            let _ = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass);
            self.draw_solids(&mut pass, frame.foregrounds.clone());
            self.draw_images(&mut pass, frame.batches, true);
        }

        self.queue.submit(Some(encoder.finish()));
        self.atlas.trim();

        slint::Image::try_from(self.texture.clone()).ok()
    }

    /// Create the target, pipelines and glyph atlas.
    fn new(device: wgpu::Device, queue: wgpu::Queue, width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let (texture, view) = Self::create_target(&device, width, height);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("slintcon-terminal-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let attributes = wgpu::vertex_attr_array![1 => Float32x4, 2 => Float32x4];
        let pipeline = |label, vertex, fragment| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attributes,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let solid_pipeline = pipeline("slintcon-solid", "vs_solid", "fs_solid");
        let image_pipeline = pipeline("slintcon-image", "vs_image", "fs_image");

        let cache = glyphon::Cache::new(&device);
        let mut atlas = TextAtlas::with_color_mode(&device, &queue, &cache, FORMAT, ColorMode::Web);
        let viewport = Viewport::new(&device, &cache);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        Self {
            instances: Self::new_instance_buffer(&device, 64),
            capacity: 64,
            device,
            queue,
            texture,
            view,
            solid_pipeline,
            image_pipeline,
            textures: HashMap::new(),
            atlas,
            viewport,
            text_renderer,
        }
    }

    /// Create the render target texture and its view.
    fn create_target(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("slintcon-terminal-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    /// An instance buffer with room for `capacity` quads.
    fn new_instance_buffer(device: &wgpu::Device, capacity: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("slintcon-instances"),
            size: capacity * size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// The render target's size in pixels.
    fn size(&self) -> (u32, u32) {
        (self.texture.width(), self.texture.height())
    }

    /// Recreate the render target at the new size.
    fn resize(&mut self, width: u32, height: u32) {
        if (width, height) == self.size() {
            return;
        }
        (self.texture, self.view) = Self::create_target(&self.device, width, height);
    }

    /// Upload `img` as a texture with its bind group.
    fn upload_image(&mut self, img: &ImagePixels) -> GpuImage {
        let texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("slintcon-image"),
                size: wgpu::Extent3d {
                    width: img.width,
                    height: img.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::default(),
            &img.rgba(),
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("slintcon-image-bind-group"),
            layout: &self.image_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        GpuImage {
            bind_group,
            opaque: img.view().is_opaque(),
        }
    }

    /// Write `quads` to the instance buffer, growing it if needed.
    fn write_instances(&mut self, quads: &[Instance]) {
        if quads.is_empty() {
            return;
        }
        let needed = quads.len() as u64;
        if needed > self.capacity {
            self.capacity = needed.next_power_of_two();
            self.instances = Self::new_instance_buffer(&self.device, self.capacity);
        }
        self.queue
            .write_buffer(&self.instances, 0, bytemuck::cast_slice(quads));
    }

    /// Draw the solid quads in `range`.
    fn draw_solids(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if !range.is_empty() {
            pass.set_pipeline(&self.solid_pipeline);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, range);
        }
    }

    /// Draw the batches above (`over`) or below the text.
    fn draw_images(&self, pass: &mut wgpu::RenderPass<'_>, batches: &[ImageBatch], over: bool) {
        pass.set_pipeline(&self.image_pipeline);
        // glyphon rebinds slot 0 for its own vertices, so bind ours every time.
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for b in batches.iter().filter(|b| b.over_text == over) {
            if let Some(image) = self.textures.get(&b.key) {
                pass.set_bind_group(0, &image.bind_group, &[]);
                pass.draw(0..6, b.start..b.start + b.count);
            }
        }
    }
}

/// The corners of the rect at `pos` of `extent` in normalized device
/// coordinates of a target of `size`.
fn ndc_rect(size: (u32, u32), pos: (f32, f32), extent: (f32, f32)) -> [f32; 4] {
    let (width, height) = (size.0 as f32, size.1 as f32);
    [
        pos.0 / width * 2.0 - 1.0,
        1.0 - pos.1 / height * 2.0,
        (pos.0 + extent.0) / width * 2.0 - 1.0,
        1.0 - (pos.1 + extent.1) / height * 2.0,
    ]
}

/// One text area per shaped segment, on a target of `size`.
fn build_text_areas(
    rows: &[Vec<(f32, Buffer)>],
    cell_height: f32,
    size: (u32, u32),
) -> Vec<TextArea<'_>> {
    let bounds = TextBounds {
        left: 0,
        top: 0,
        right: size.0 as i32,
        bottom: size.1 as i32,
    };
    let mut areas = Vec::new();
    for (line, row) in rows.iter().enumerate() {
        // Integer `left`/`top` (cell and cell_height are whole) and a scale
        // of 1 keep glyphon's glyph origins on whole pixels, matching the
        // CPU path.
        for (left, buffer) in row {
            areas.push(TextArea {
                buffer,
                left: *left,
                top: line as f32 * cell_height,
                scale: 1.0,
                bounds,
                default_color: glyphon::Color::rgba(255, 255, 255, 255),
                custom_glyphs: &[],
            });
        }
    }
    areas
}
