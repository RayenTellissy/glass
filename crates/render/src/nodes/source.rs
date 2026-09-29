use media::Primaries;

use crate::colour::WorkingSpace;
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::interop::ImportedFrame;
use crate::upload::UploadedFrame;
use crate::yuv::{YuvOutput, YuvParams, YuvPlanes};

/// A decoded frame's Y'CbCr planes into the working space: linear light, premultiplied
/// (opaque), in the working primaries, at the frame's luma size. One pass: the matrix,
/// range, chroma siting and transfer of [`crate::yuv`] plus the gamut matrix.
#[derive(Clone, Copy, Debug)]
pub struct YuvSourceNode<'a> {
    planes: YuvPlanes<'a>,
    params: YuvParams,
    primaries: Primaries,
    width: u32,
    height: u32,
}

impl<'a> YuvSourceNode<'a> {
    /// Planes described by `params`, whose linear output is in `primaries`.
    pub fn new(
        planes: YuvPlanes<'a>,
        params: YuvParams,
        primaries: Primaries,
        working: WorkingSpace,
    ) -> Self {
        let luma = match planes {
            YuvPlanes::Biplanar { luma, .. } => luma,
            YuvPlanes::Planar { y, .. } => y,
        };
        let size = luma.texture().size();
        Self {
            planes,
            params: params
                .with_output(YuvOutput::Linear)
                .with_gamut(&working.from(primaries)),
            primaries,
            width: size.width,
            height: size.height,
        }
    }

    /// A software-decoded frame uploaded with [`crate::upload::upload_frame`].
    pub fn uploaded(frame: &'a UploadedFrame, working: WorkingSpace) -> Self {
        Self::new(frame.planes(), frame.params, frame.primaries, working)
    }

    /// A hardware frame imported with [`crate::interop::SurfaceImporter`].
    pub fn imported(frame: &'a ImportedFrame, working: WorkingSpace) -> Self {
        let hw = frame.frame();
        Self::new(
            YuvPlanes::Biplanar {
                luma: &frame.luma,
                chroma: &frame.chroma,
            },
            YuvParams::for_frame(hw),
            hw.colour_space().primaries,
            working,
        )
    }
}

impl Node for YuvSourceNode<'_> {
    fn label(&self) -> &str {
        "source"
    }

    fn input_count(&self) -> usize {
        0
    }

    fn outputs(&self, _inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![TextureDesc::working(self.width, self.height)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("size", format!("{}x{}", self.width, self.height)),
            ("matrix", self.params.matrix.name().to_owned()),
            ("transfer", self.params.transfer.name().to_owned()),
            ("primaries", self.primaries.name().to_owned()),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        _inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let timestamps = cx.timestamps();
        cx.renderer.yuv().encode_timed(
            cx.renderer.device(),
            cx.encoder,
            self.planes,
            &self.params,
            outputs[0],
            TextureDesc::WORKING_FORMAT,
            timestamps,
        );
    }
}
