//! A custom iced `shader` widget that draws an RGBA frame from a *persistent*
//! wgpu texture on iced's own device.
//!
//! Why not the `image` widget: `image::Handle::from_rgba` mints a new handle id
//! every frame, so iced's image cache inserts+evicts a fresh texture each redraw
//! — that thrash flickers (especially during animated transitions). Here we own
//! ONE texture and `write_texture` into it only when the frame actually changed
//! (tracked by a content hash used as the version). This is the same approach
//! finn and iced_video_player use. The frame is letterboxed (Contain) to the
//! widget via a scale uniform, matching `map_cursor` in main.rs.

use std::sync::Arc;

use iced::advanced::graphics::Viewport;
use iced::wgpu;
use iced::widget::shader::{self, Primitive};
use iced::{mouse, Rectangle};

/// A decoded RGBA8 frame (straight alpha, tightly packed `width * 4` rows).
pub struct FrameData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Handed to the `shader` widget each `view()`. `version` is a content hash, so
/// identical frames don't trigger a GPU upload.
pub struct FrameProgram {
    pub version: u64,
    pub frame: Arc<FrameData>,
}

impl<Message> shader::Program<Message> for FrameProgram {
    type State = ();
    type Primitive = FramePrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> FramePrimitive {
        FramePrimitive {
            version: self.version,
            frame: self.frame.clone(),
        }
    }
}

#[derive(Clone)]
pub struct FramePrimitive {
    version: u64,
    frame: Arc<FrameData>,
}

impl std::fmt::Debug for FramePrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramePrimitive")
            .field("version", &self.version)
            .field("size", &(self.frame.width, self.frame.height))
            .finish()
    }
}

impl Primitive for FramePrimitive {
    type Pipeline = FramePipeline;

    fn prepare(
        &self,
        pipeline: &mut FramePipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let scale = viewport.scale_factor();
        let area_w = (bounds.width * scale).max(1.0);
        let area_h = (bounds.height * scale).max(1.0);
        pipeline.prepare(device, queue, self.version, &self.frame, area_w, area_h);
    }

    fn render(
        &self,
        pipeline: &FramePipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        pipeline.render(encoder, target, clip_bounds);
    }
}

/// Shared GPU state: one render pipeline + sampler, plus a single persistent
/// target (texture/bind group/uniform) reused across frames and movies.
#[derive(Debug)]
pub struct FramePipeline {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    target: Option<Target>,
}

#[derive(Debug)]
struct Target {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    width: u32,
    height: u32,
    version: u64,
}

impl shader::Pipeline for FramePipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ruffle frame shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ruffle frame bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ruffle frame pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ruffle frame pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ruffle frame sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        FramePipeline {
            pipeline,
            sampler,
            layout,
            target: None,
        }
    }
}

impl FramePipeline {
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        version: u64,
        frame: &FrameData,
        area_w: f32,
        area_h: f32,
    ) {
        let needs_texture = match &self.target {
            Some(t) => t.width != frame.width || t.height != frame.height,
            None => true,
        };

        if needs_texture {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ruffle frame tex"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ruffle frame uniform"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ruffle frame bg"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.target = Some(Target {
                texture,
                bind_group,
                uniform,
                width: frame.width,
                height: frame.height,
                version: u64::MAX, // force the upload below
            });
        }

        let target = self.target.as_mut().unwrap();

        if target.version != version {
            target.version = version;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        // Letterbox (Contain): shrink the NDC quad on the longer axis so the
        // frame fits the widget while preserving aspect ratio.
        let area_aspect = area_w / area_h;
        let tex_aspect = frame.width as f32 / frame.height as f32;
        let (sx, sy) = if tex_aspect > area_aspect {
            (1.0, area_aspect / tex_aspect)
        } else {
            (tex_aspect / area_aspect, 1.0)
        };
        queue.write_buffer(
            &target.uniform,
            0,
            bytemuck::cast_slice(&[sx, sy, 0.0f32, 0.0f32]),
        );
    }

    fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let Some(t) = &self.target else {
            return;
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ruffle frame pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load, // preserve the black letterbox background
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        pass.set_viewport(
            clip_bounds.x as f32,
            clip_bounds.y as f32,
            clip_bounds.width as f32,
            clip_bounds.height as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(
            clip_bounds.x,
            clip_bounds.y,
            clip_bounds.width,
            clip_bounds.height,
        );
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &t.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

const SHADER: &str = r#"
struct Uniforms { scale: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),  vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    var out: VsOut;
    out.pos = vec4<f32>(c * u.scale, 0.0, 1.0);
    out.uv = vec2<f32>(c.x * 0.5 + 0.5, 1.0 - (c.y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;
