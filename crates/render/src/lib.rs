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

#[allow(unsafe_code)]
pub mod interop;
pub mod upload;
pub mod yuv;

/// The wgpu this crate is built against (the version egui-wgpu uses), so callers that
/// hand `render` a device do not have to pin it separately.
pub use wgpu;
