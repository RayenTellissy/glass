//! GPU rendering for Glass: wgpu device and queue ownership, the render graph compiler,
//! compositor nodes, WGSL shaders, text, masks, LUTs, scopes, the VRAM frame cache, the
//! async readback ring, import of platform frame handles as textures, and the
//! presentation API the UI uses to show the preview. `unsafe` is allowed only in
//! `render::interop`.
//!
//! Decoded video reaches the GPU in two steps: [`interop::SurfaceImporter`] wraps a
//! hardware surface ([`media::HwFrame`]) as one wgpu texture per plane without a copy (or
//! [`upload::upload_frame`] writes a software-decoded frame's planes), and
//! [`yuv::YuvConverter`] turns those planes into linear or encoded RGBA with the frame's
//! [`media::ColourSpace`].

//!
//! Each frame is described as a [`graph::Graph`] of [`nodes`], compiled (topological
//! order, culling, intermediate aliasing through a [`graph::TexturePool`]) and executed
//! with a [`renderer::Renderer`], timed per pass by a [`profiler::GpuProfiler`]. Layers
//! are stacked by [`nodes::CompositeNode`] with the [`blend::BlendMode`]s, whose CPU
//! reference lives in [`blend`]; each layer is sampled through its [`transform`], cut by its
//! [`crop`] and scaled by its opacity inside the same pass. Vector shapes ([`shape`]) are
//! rasterised as exact coverage by [`shape::ShapeNode`] into a layer, or by
//! [`shape::CoverageNode`] for masks. A layer's [`mask`]s (shapes unioned, feathered,
//! inverted, combined by union or intersection) multiply it in [`nodes::MaskNode`] before
//! it is composited.
//!
//! Rendered preview frames are kept by [`frame_cache::FrameCache`]: an LRU of textures in
//! VRAM that spills to RAM through the async readback ring.
//!
//! Looks come from [`lut`] files (`.cube`), uploaded once as a [`nodes::GpuLut`] and
//! applied by [`nodes::LutNode`] in the LUT's declared input space with an intensity mix.
//!
//! A clip whose source is offline shows a [`slate`]: rendered once per size and label,
//! cached, and imported into each frame's graph like a decoded frame.

pub mod blend;
pub mod colour;
pub mod crop;
pub mod frame_cache;
pub mod graph;
#[allow(unsafe_code)]
pub mod interop;
pub mod lut;
pub mod mask;
pub mod nodes;
pub mod profiler;
pub mod readback;
pub mod renderer;
pub mod shaders;
pub mod shape;
pub mod slate;
pub mod text;
pub mod transform;
pub mod upload;
pub mod yuv;

/// The wgpu this crate is built against (the version egui-wgpu uses), so callers that
/// hand `render` a device do not have to pin it separately.
pub use wgpu;
