//! wgpu renderer: a single instanced-quad pipeline drawing rectangles and
//! glyphs from the atlas, in submission order (painter's algorithm).

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::font::{ATLAS_SIZE, FontSystem, Glyph};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Instance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
    pub color: [f32; 4],
    pub kind: u32,
    pub radius: f32,
    pub thickness: f32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    screen: [f32; 2],
    atlas: [f32; 2],
}

pub const KIND_RECT: u32 = 0;
pub const KIND_MASK: u32 = 1;
pub const KIND_COLOR: u32 = 2;
pub const KIND_OUTLINE: u32 = 3;
pub const KIND_PACMAN: u32 = 4;

/// Multiply a color's RGB toward black/white; `f` < 1 darkens.
pub fn scale_rgb(c: Rgba, f: f32) -> Rgba {
    [(c[0] * f).min(1.0), (c[1] * f).min(1.0), (c[2] * f).min(1.0), c[3]]
}

pub type Rgba = [f32; 4];

pub fn rgb(c: [u8; 3]) -> Rgba {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]
}

pub fn with_alpha(mut c: Rgba, a: f32) -> Rgba {
    c[3] = a;
    c
}

/// Accumulates draw instances for one frame.
#[derive(Default)]
pub struct Batch {
    pub instances: Vec<Instance>,
}

impl Batch {
    pub fn clear(&mut self) {
        self.instances.clear();
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        self.rrect(x, y, w, h, 0.0, color);
    }

    /// Filled rectangle with rounded corners.
    pub fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Rgba) {
        if w <= 0.0 || h <= 0.0 || color[3] <= 0.0 {
            return;
        }
        self.instances.push(Instance {
            pos: [x, y],
            size: [w, h],
            uv0: [0.0; 2],
            uv1: [0.0; 2],
            color,
            kind: KIND_RECT,
            radius,
            thickness: 0.0,
            _pad: 0,
        });
    }

    /// A Pac-Man disc filling the rect: `facing` in radians (0 = right,
    /// PI = left), `mouth` = half-angle of the open mouth in radians.
    pub fn pacman(&mut self, x: f32, y: f32, size: f32, facing: f32, mouth: f32, color: Rgba) {
        self.instances.push(Instance {
            pos: [x, y],
            size: [size, size],
            uv0: [facing, 0.0],
            uv1: [facing, 0.0],
            color,
            kind: KIND_PACMAN,
            radius: size * 0.5,
            thickness: mouth,
            _pad: 0,
        });
    }

    /// Rounded outline of `thickness` pixels, drawn inside the given rect.
    pub fn outline(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, thickness: f32, color: Rgba) {
        if w <= 0.0 || h <= 0.0 || color[3] <= 0.0 {
            return;
        }
        self.instances.push(Instance {
            pos: [x, y],
            size: [w, h],
            uv0: [0.0; 2],
            uv1: [0.0; 2],
            color,
            kind: KIND_OUTLINE,
            radius,
            thickness,
            _pad: 0,
        });
    }

    /// Place a glyph with its origin (baseline-left) at (ox, oy).
    pub fn glyph(&mut self, ox: f32, oy: f32, g: &Glyph, color: Rgba) {
        let x = ox + g.left as f32;
        let y = oy - g.top as f32;
        self.instances.push(Instance {
            pos: [x, y],
            size: [g.w as f32, g.h as f32],
            uv0: [g.x as f32, g.y as f32],
            uv1: [(g.x + g.w) as f32, (g.y + g.h) as f32],
            color,
            kind: if g.colored { KIND_COLOR } else { KIND_MASK },
            radius: 0.0,
            thickness: 0.0,
            _pad: 0,
        });
    }
}

/// GPU objects shared by every window: one instance, adapter, device, queue.
pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub adapter_name: String,
}

impl Gpu {
    /// Create the shared context, picking an adapter that can present to
    /// `window`.
    pub fn new(window: &Arc<Window>) -> Result<Arc<Self>> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        if std::env::var_os("WGPU_BACKEND").is_none() {
            desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        }
        let instance = wgpu::Instance::new(desc);
        let probe = instance.create_surface(window.clone()).context("creating surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&probe),
            ..Default::default()
        }))
        .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;
        drop(probe);
        log::info!("GPU: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
        let adapter_name = adapter.get_info().name.clone();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kindlyTerm"),
            ..Default::default()
        }))
        .context("requesting device")?;
        Ok(Arc::new(Self { instance, adapter, device, queue, adapter_name }))
    }
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals_buf: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    instance_buf: wgpu::Buffer,
    instance_cap: usize,
    pub width: u32,
    pub height: u32,
    /// Set by `resize`; the surface is reconfigured lazily in `render` so a
    /// burst of resize events costs one swapchain rebuild, not one per event.
    needs_configure: bool,
}

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let surface = gpu.instance.create_surface(window.clone()).context("creating surface")?;
        if !gpu.adapter.is_surface_supported(&surface) {
            return Err(anyhow!("the GPU adapter cannot present to this window"));
        }
        let adapter = &gpu.adapter;
        let device = &gpu.device;

        let caps = surface.get_capabilities(adapter);
        // Prefer a non-sRGB format so our colors pass through untouched.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        // Mailbox never blocks in acquire/present, which matters during
        // interactive resizes on Wayland where FIFO can stall a whole
        // compositor cycle per frame. We only redraw on demand, so Mailbox
        // does not spin. Fall back to FIFO where Mailbox is unavailable.
        log::info!("present modes: {:?}, formats: {:?}", caps.present_modes, caps.formats);
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        // Prefer a mode that lets the window be translucent.
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Inherit) {
            wgpu::CompositeAlphaMode::Inherit
        } else {
            caps.alpha_modes[0]
        };
        log::info!("alpha modes: {:?} -> using {:?}", caps.alpha_modes, alpha_mode);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
            ..surface.get_default_config(adapter, size.width.max(1), size.height.max(1)).unwrap()
        };
        surface.configure(device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kindlyterm shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: ATLAS_SIZE, height: ATLAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let globals_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("globals"),
            contents: bytemuck::bytes_of(&Globals {
                screen: [config.width as f32, config.height as f32],
                atlas: [ATLAS_SIZE as f32, ATLAS_SIZE as f32],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layout"),
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x2,
                        3 => Float32x2, 4 => Float32x4, 5 => Uint32,
                        6 => Float32, 7 => Float32
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let instance_cap = 1 << 14;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (instance_cap * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            gpu: Arc::clone(&gpu),
            surface,
            config,
            pipeline,
            bind_group,
            globals_buf,
            atlas_tex,
            instance_buf,
            instance_cap,
            width: size.width,
            height: size.height,
            needs_configure: false,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.config.width = width;
        self.config.height = height;
        self.needs_configure = true;
        self.gpu.queue.write_buffer(
            &self.globals_buf,
            0,
            bytemuck::bytes_of(&Globals {
                screen: [width as f32, height as f32],
                atlas: [ATLAS_SIZE as f32, ATLAS_SIZE as f32],
            }),
        );
    }

    /// Push any newly rasterized glyphs to the GPU atlas.
    fn upload_atlas(&self, fonts: &mut FontSystem) {
        for (x, y, w, h) in fonts.dirty.drain(..) {
            // Copy the dirty rect out row by row into a tight buffer.
            let mut data = Vec::with_capacity((w * h * 4) as usize);
            for row in y..y + h {
                let start = ((row * ATLAS_SIZE + x) * 4) as usize;
                data.extend_from_slice(&fonts.atlas[start..start + (w * 4) as usize]);
            }
            self.gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
    }

    /// Debug helper: render the batch to an offscreen texture and save a PNG.
    pub fn screenshot(&mut self, fonts: &mut FontSystem, batch: &Batch, clear: Rgba, path: &std::path::Path) -> Result<()> {
        self.upload_atlas(fonts);
        self.upload_instances(batch);
        let (w, h) = (self.width, self.height);
        let tex = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bytes_per_row = (w * 4).div_ceil(256) * 256;
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot readback"),
            size: (bytes_per_row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shot") });
        self.encode_pass(&mut encoder, &view, batch, clear);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit(Some(encoder.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        rx.recv()??;
        let data = slice.get_mapped_range().map_err(|e| anyhow!("map range: {e:?}"))?;
        let bgra = matches!(self.config.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bytes_per_row) as usize;
            let row_data = &data[start..start + (w * 4) as usize];
            for px in row_data.chunks(4) {
                if bgra { pixels.extend_from_slice(&[px[2], px[1], px[0], 255]) } else { pixels.extend_from_slice(&[px[0], px[1], px[2], 255]) }
            }
        }
        drop(data);
        buf.unmap();
        let file = std::fs::File::create(path)?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&pixels)?;
        Ok(())
    }

    fn upload_instances(&mut self, batch: &Batch) {
        if batch.instances.len() > self.instance_cap {
            self.instance_cap = batch.instances.len().next_power_of_two();
            self.instance_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: (self.instance_cap * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !batch.instances.is_empty() {
            self.gpu.queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&batch.instances));
        }
    }

    fn encode_pass(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, batch: &Batch, clear: Rgba) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Premultiplied clear: rgb * alpha, alpha = opacity.
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: (clear[0] * clear[3]) as f64,
                        g: (clear[1] * clear[3]) as f64,
                        b: (clear[2] * clear[3]) as f64,
                        a: clear[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if !batch.instances.is_empty() {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buf.slice(..));
            pass.draw(0..6, 0..batch.instances.len() as u32);
        }
    }

    pub fn render(&mut self, fonts: &mut FontSystem, batch: &Batch, clear: Rgba) -> Result<()> {
        self.upload_atlas(fonts);
        self.upload_instances(batch);

        if self.needs_configure {
            let t0 = std::time::Instant::now();
            self.surface.configure(&self.gpu.device, &self.config);
            self.needs_configure = false;
            let dt = t0.elapsed().as_secs_f64() * 1e3;
            if dt > 4.0 {
                log::debug!("surface configure took {dt:.1}ms");
            }
        }
        let t0 = std::time::Instant::now();
        let acquired = self.surface.get_current_texture();
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        if dt > 4.0 {
            log::debug!("get_current_texture took {dt:.1}ms");
        }
        let frame = match acquired {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                // Rebuild and retry once so a resize never shows a stale frame.
                self.surface.configure(&self.gpu.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    _ => return Ok(()),
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
            other => return Err(anyhow!("surface error: {other:?}")),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        self.encode_pass(&mut encoder, &view, batch, clear);
        self.gpu.queue.submit(Some(encoder.finish()));
        let t0 = std::time::Instant::now();
        self.gpu.queue.present(frame);
        let dt = t0.elapsed().as_secs_f64() * 1e3;
        if dt > 4.0 {
            log::debug!("present took {dt:.1}ms");
        }
        Ok(())
    }
}
