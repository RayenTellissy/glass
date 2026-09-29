use media::Primaries;

use crate::colour::{self, Mat3};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::renderer::{Effect, Filter, Uniform};

/// Linear RGB from one set of primaries into another (for example a Display P3 still
/// into the BT.709 working space, or the working space into an output's primaries).
#[derive(Clone, Debug)]
pub struct GamutNode {
    from: Primaries,
    to: Primaries,
    matrix: Mat3,
}

impl GamutNode {
    pub fn new(from: Primaries, to: Primaries) -> Self {
        Self {
            from,
            to,
            matrix: colour::gamut_matrix(from, to),
        }
    }

    pub fn matrix(&self) -> &Mat3 {
        &self.matrix
    }
}

impl Node for GamutNode {
    fn label(&self) -> &str {
        "gamut"
    }

    fn input_count(&self) -> usize {
        1
    }

    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![inputs[0].with_format(TextureDesc::WORKING_FORMAT)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("from", self.from.name().to_owned()),
            ("to", self.to.name().to_owned()),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let uniform = colour::uniform_rows(&self.matrix)
            .into_iter()
            .fold(Uniform::new(), Uniform::vec4);
        cx.draw(
            &Effect {
                label: "gamut",
                shader: "gamut",
                entry: "fs",
                uniform: uniform.bytes(),
                inputs,
                filter: Filter::Nearest,
            },
            outputs[0],
        );
    }
}
