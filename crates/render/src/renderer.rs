//! The compositor's shared GPU state: device and queue handles, the effect pipelines
//! (built lazily per shader, entry point, target format and input count, and rebuilt when
//! a debug build reloads their shader), samplers and the Y'CbCr converter. One per device, owned by the render thread; nodes reach it
//! through [`crate::graph::NodeContext`].

use std::collections::HashMap;
use std::sync::Mutex;

use wgpu::util::DeviceExt;

use crate::shaders::ShaderLibrary;
use crate::yuv::YuvConverter;

/// Identifies one effect pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectKey {
    pub shader: &'static str,
    pub entry: &'static str,
    pub format: wgpu::TextureFormat,
    pub inputs: u8,
}

/// Shader, name and target format of a pipeline a node builds itself.
type CustomKey = (&'static str, &'static str, wgpu::TextureFormat);

/// Which sampler binding 1 holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    #[default]
    Linear,
}

/// One full-screen effect draw: `common.wgsl` + `shader`, entry point `entry`, a uniform
/// at binding 0, a sampler at binding 1 and `inputs` at bindings 2 and up.
pub struct Effect<'a> {
    pub label: &'a str,
    pub shader: &'static str,
    pub entry: &'static str,
    pub uniform: &'a [u8],
    pub inputs: &'a [&'a wgpu::TextureView],
    pub filter: Filter,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layouts: Mutex<HashMap<u8, wgpu::BindGroupLayout>>,
    pipelines: Mutex<HashMap<EffectKey, wgpu::RenderPipeline>>,
    custom: Mutex<HashMap<CustomKey, wgpu::RenderPipeline>>,
    shaders: ShaderLibrary,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    yuv: YuvConverter,
    dummy: wgpu::TextureView,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer").finish_non_exhaustive()
    }
}

impl Renderer {
    /// Uses [`ShaderLibrary::default`]: watched shader files in debug builds, the
    /// embedded ones in release builds.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_shaders(device, queue, ShaderLibrary::default())
    }

    pub fn with_shaders(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        shaders: ShaderLibrary,
    ) -> Self {
        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("effect"),
                mag_filter: filter,
                min_filter: filter,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            })
        };
        Self {
            device: device.clone(),
            queue: queue.clone(),
            layouts: Mutex::default(),
            pipelines: Mutex::default(),
            custom: Mutex::default(),
            shaders,
            linear: sampler(wgpu::FilterMode::Linear),
            nearest: sampler(wgpu::FilterMode::Nearest),
            yuv: YuvConverter::new(device),
            dummy: device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("unbound input"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default()),
        }
    }

    /// A 1x1 texture for texture bindings a draw leaves unused.
    pub fn dummy_view(&self) -> &wgpu::TextureView {
        &self.dummy
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn yuv(&self) -> &YuvConverter {
        &self.yuv
    }

    pub fn shaders(&self) -> &ShaderLibrary {
        &self.shaders
    }

    /// Drops the pipelines of shaders reloaded since the last frame (all of them when
    /// `common.wgsl` changed) so they rebuild from the new source on first use. Returns
    /// the reloaded shader names. Called once per frame by
    /// [`crate::graph::CompiledGraph::record`].
    pub fn begin_frame(&self) -> Vec<String> {
        let changed = self.shaders.poll_changes();
        if !changed.is_empty() {
            let all = changed.iter().any(|n| n == "common");
            self.pipelines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|key, _| !all && !changed.iter().any(|n| n == key.shader));
            self.custom
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|key, _| !all && !changed.iter().any(|n| n == key.0));
        }
        changed
    }

    fn layout(&self, inputs: u8) -> wgpu::BindGroupLayout {
        let mut layouts = self.layouts.lock().unwrap_or_else(|e| e.into_inner());
        layouts
            .entry(inputs)
            .or_insert_with(|| {
                let mut entries = vec![
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
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
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ];
                entries.extend((0..u32::from(inputs)).map(|i| wgpu::BindGroupLayoutEntry {
                    binding: 2 + i,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }));
                self.device
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("effect"),
                        entries: &entries,
                    })
            })
            .clone()
    }

    /// The pipeline for `key`, built on first use.
    pub fn pipeline(&self, key: EffectKey) -> wgpu::RenderPipeline {
        if let Some(p) = self
            .pipelines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return p.clone();
        }
        let layout = self.layout(key.inputs);
        let pipeline = self.build(key, &layout);
        self.pipelines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, pipeline.clone());
        pipeline
    }

    /// A pipeline a node describes itself (its own vertex stage, bindings or blending),
    /// made by `build` from `shader` (with `common.wgsl` prepended) on first use, cached
    /// under `name` and `format`, and dropped like the effect pipelines when `shader`
    /// reloads.
    pub fn custom_pipeline(
        &self,
        shader: &'static str,
        name: &'static str,
        format: wgpu::TextureFormat,
        build: impl FnOnce(&wgpu::Device, &wgpu::ShaderModule) -> wgpu::RenderPipeline,
    ) -> wgpu::RenderPipeline {
        let key = (shader, name, format);
        if let Some(p) = self
            .custom
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return p.clone();
        }
        let source = self.shaders.composed(shader).unwrap_or_default();
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(shader),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline = build(&self.device, &module);
        self.custom
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, pipeline.clone());
        pipeline
    }

    fn build(&self, key: EffectKey, layout: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
        let source = self.shaders.composed(key.shader).unwrap_or_default();
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(key.shader),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(key.shader),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(key.shader),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_fullscreen"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(key.entry),
                    compilation_options: Default::default(),
                    targets: &[Some(key.format.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
    }

    /// Records `effect` as one render pass that overwrites every pixel of `target`.
    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        effect: &Effect<'_>,
        target: &wgpu::TextureView,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let format = target.texture().format();
        let inputs = u8::try_from(effect.inputs.len()).unwrap_or(u8::MAX);
        let pipeline = self.pipeline(EffectKey {
            shader: effect.shader,
            entry: effect.entry,
            format,
            inputs,
        });
        let mut contents = effect.uniform.to_vec();
        contents.resize(contents.len().next_multiple_of(16).max(16), 0);
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(effect.label),
                contents: &contents,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let sampler = match effect.filter {
            Filter::Nearest => &self.nearest,
            Filter::Linear => &self.linear,
        };
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ];
        entries.extend(effect.inputs.iter().zip(2u32..).map(|(view, binding)| {
            wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            }
        }));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(effect.label),
            layout: &self.layout(inputs),
            entries: &entries,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(effect.label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: timestamps,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Little-endian bytes of a uniform built from `f32`/`u32` words.
#[derive(Default)]
pub struct Uniform(Vec<u8>);

impl Uniform {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn f32(mut self, v: f32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn vec4(self, v: [f32; 4]) -> Self {
        v.into_iter().fold(self, Self::f32)
    }

    /// Zero words up to the next 16-byte boundary (WGSL `vec4` alignment).
    pub fn align16(mut self) -> Self {
        self.0.resize(self.0.len().next_multiple_of(16), 0);
        self
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
