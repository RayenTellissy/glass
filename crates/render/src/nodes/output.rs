//! The last node of a frame: the working space (linear light relative to 203-nit
//! reference white, premultiplied, in the project primaries) to a delivery encoding.
//! Intermediates stay `Rgba16Float`; only this node writes 8 and 10-bit targets, which it
//! dithers so slow gradients do not band.

use media::Primaries;

use crate::colour::{self, Mat3, WorkingSpace};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::renderer::{Effect, Filter, Uniform};

/// The output encoding's transfer function. Linear light is relative to SDR reference
/// white (1.0 = 203 nits), as in the working space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputTransfer {
    /// IEC 61966-2-1 piecewise sRGB: the preview on sRGB displays, stills.
    #[default]
    Srgb,
    /// The inverse of the BT.1886 EOTF (a 2.4 power, zero black): SDR video tagged
    /// BT.709, which players decode with BT.1886, so display light round-trips.
    Bt1886,
    /// The BT.709 camera OETF, for scene-referred delivery; decoded with BT.1886 it adds
    /// the usual end-to-end gamma of about 1.2.
    Bt709,
    /// SMPTE ST 2084, absolute: 1.0 = 203 nits, 10,000 nits is about 49.3.
    Pq,
    /// ARIB STD-B67 after the inverse BT.2100 OOTF for a 1000-nit display.
    Hlg,
    /// No transfer (float outputs, scopes).
    Linear,
}

impl OutputTransfer {
    fn code(self) -> u32 {
        match self {
            Self::Srgb => 0,
            Self::Bt1886 => 1,
            Self::Bt709 => 2,
            Self::Pq => 3,
            Self::Hlg => 4,
            Self::Linear => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::Bt1886 => "BT.1886",
            Self::Bt709 => "BT.709",
            Self::Pq => "PQ",
            Self::Hlg => "HLG",
            Self::Linear => "linear",
        }
    }

    /// The encoded value of one linear component (HLG needs all three, see
    /// [`Self::encode_rgb`]). The CPU reference for the output shader.
    pub fn encode(self, v: f64) -> f64 {
        let v = v.max(0.0);
        match self {
            Self::Srgb => {
                if v <= 0.003_130_8 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }
            Self::Bt1886 => v.powf(1.0 / 2.4),
            Self::Bt709 => {
                if v < 0.018 {
                    4.5 * v
                } else {
                    1.099 * v.powf(0.45) - 0.099
                }
            }
            Self::Pq => media::colour::pq_inverse_eotf(v * media::colour::REFERENCE_WHITE_NITS),
            Self::Hlg => self.encode_rgb([v; 3])[0],
            Self::Linear => v,
        }
    }

    pub fn encode_rgb(self, rgb: [f64; 3]) -> [f64; 3] {
        if self != Self::Hlg {
            return rgb.map(|v| self.encode(v));
        }
        let peak = media::colour::HLG_PEAK_NITS;
        let fd = rgb.map(|v| v.max(0.0) * media::colour::REFERENCE_WHITE_NITS / peak);
        let yd = 0.2627 * fd[0] + 0.6780 * fd[1] + 0.0593 * fd[2];
        if yd <= 0.0 {
            return [0.0; 3];
        }
        let gamma = media::colour::HLG_SYSTEM_GAMMA;
        let ys = yd.powf(1.0 / gamma);
        fd.map(|f| {
            let e = (f / ys.powf(gamma - 1.0)).clamp(0.0, 1.0);
            const A: f64 = 0.178_832_77;
            const B: f64 = 1.0 - 4.0 * A;
            const C: f64 = 0.559_910_73;
            if e <= 1.0 / 12.0 {
                (3.0 * e).sqrt()
            } else {
                A * (12.0 * e - B).ln() + C
            }
        })
    }
}

/// How quantisation error is shaped before an integer target stores the value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dither {
    None,
    /// Triangular-PDF noise of plus or minus one code value (two 16-bit uniform
    /// variables per channel from one PCG3D hash of pixel and frame): removes banding and
    /// makes the error independent of the signal. Exact black and white stay exact.
    #[default]
    Triangular,
}

/// Working space to an output texture: gamut into the output primaries, alpha divided
/// out (straight alpha, the delivery convention) unless asked to keep it premultiplied,
/// the transfer, then dither for integer formats.
///
/// Targets: `Rgba8Unorm`/`Bgra8Unorm` (8-bit SDR, dithered), `Rgb10a2Unorm` and
/// `Rgba16Unorm` (10 and 16-bit, dithered to their depth; PQ and HLG deliveries),
/// `Rgba16Float` (no dither).
#[derive(Clone, Debug)]
pub struct OutputNode {
    format: wgpu::TextureFormat,
    transfer: OutputTransfer,
    primaries: Primaries,
    gamut: Mat3,
    dither: Dither,
    seed: u32,
    straight_alpha: bool,
}

impl OutputNode {
    pub fn new(
        working: WorkingSpace,
        primaries: Primaries,
        transfer: OutputTransfer,
        format: wgpu::TextureFormat,
    ) -> Self {
        Self {
            format,
            transfer,
            primaries,
            gamut: colour::gamut_matrix(working.primaries, primaries),
            dither: Dither::default(),
            seed: 0,
            straight_alpha: true,
        }
    }

    /// 8-bit sRGB for the preview.
    pub fn preview(working: WorkingSpace) -> Self {
        Self::new(
            working,
            Primaries::Bt709,
            OutputTransfer::Srgb,
            wgpu::TextureFormat::Rgba8Unorm,
        )
    }

    pub fn with_dither(self, dither: Dither) -> Self {
        Self { dither, ..self }
    }

    /// Varies the dither pattern per frame (use the frame number), so it does not sit
    /// still over the picture.
    pub fn with_seed(self, seed: u32) -> Self {
        Self { seed, ..self }
    }

    pub fn with_premultiplied_alpha(self) -> Self {
        Self {
            straight_alpha: false,
            ..self
        }
    }

    /// Code values per channel of the target, or `None` for float targets.
    pub fn levels(format: wgpu::TextureFormat) -> Option<u32> {
        use wgpu::TextureFormat as F;
        match format {
            F::Rgba8Unorm | F::Bgra8Unorm => Some(255),
            F::Rgb10a2Unorm => Some(1023),
            F::Rgba16Unorm => Some(65535),
            _ => None,
        }
    }
}

impl Node for OutputNode {
    fn label(&self) -> &str {
        "output"
    }

    fn input_count(&self) -> usize {
        1
    }

    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![inputs[0].with_format(self.format)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("transfer", self.transfer.name().to_owned()),
            ("primaries", self.primaries.name().to_owned()),
            ("format", format!("{:?}", self.format)),
            ("dither", format!("{:?}", self.dither)),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let levels = Self::levels(self.format).unwrap_or(0) as f32;
        let uniform = colour::uniform_rows(&self.gamut)
            .into_iter()
            .fold(Uniform::new(), Uniform::vec4)
            .u32(self.transfer.code())
            .f32(levels)
            .u32(u32::from(self.dither == Dither::Triangular))
            .u32(self.seed)
            .u32(u32::from(self.straight_alpha))
            .align16();
        cx.draw(
            &Effect {
                label: "output",
                shader: "output",
                entry: "fs",
                uniform: uniform.bytes(),
                inputs,
                filter: Filter::Nearest,
            },
            outputs[0],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media::TransferFn;

    fn near(got: f64, want: f64, tol: f64) {
        assert!((got - want).abs() <= tol, "{got} vs {want}");
    }

    #[test]
    fn pq_spot_values() {
        // SMPTE ST 2084 code values for display luminance; 203 nits is BT.2408's 58%.
        let nits = |n: f64| OutputTransfer::Pq.encode(n / media::colour::REFERENCE_WHITE_NITS);
        near(nits(0.0), 7.3e-7, 1e-6);
        near(nits(100.0), 0.508_08, 5e-5);
        near(nits(203.0), 0.580_69, 5e-5);
        near(nits(1000.0), 0.751_83, 5e-5);
        near(nits(10_000.0), 1.0, 1e-9);
    }

    #[test]
    fn hlg_spot_values() {
        // Reference white (203 nits) is HLG 75% (BT.2408), the 1000-nit peak is 100%.
        let hlg = OutputTransfer::Hlg;
        near(hlg.encode(1.0), 0.75, 1e-3);
        near(hlg.encode(1000.0 / 203.0), 1.0, 1e-7);
        near(hlg.encode(0.0), 0.0, 0.0);
        // Chromatic values come back through media's decoder (inverse OETF + OOTF).
        let rgb = [0.9, 0.3, 0.05];
        let back = TransferFn::Hlg.to_linear_rgb(hlg.encode_rgb(rgb));
        for (b, v) in back.iter().zip(rgb) {
            near(*b, v, 1e-9);
        }
    }

    #[test]
    fn sdr_transfers_invert_media_decoders() {
        for (out, decode) in [
            (OutputTransfer::Srgb, TransferFn::Srgb),
            (OutputTransfer::Bt1886, TransferFn::Bt1886),
            (OutputTransfer::Pq, TransferFn::Pq),
            (OutputTransfer::Linear, TransferFn::Linear),
        ] {
            for i in 0..=100 {
                let v = f64::from(i) / 100.0;
                near(decode.to_linear(out.encode(v)), v, 1e-9);
            }
        }
        // 18% grey.
        near(OutputTransfer::Srgb.encode(0.18), 0.461_356, 1e-6);
        near(OutputTransfer::Bt1886.encode(0.18), 0.489_437, 1e-6);
        near(OutputTransfer::Bt709.encode(0.18), 0.409_008, 1e-6);
    }

    #[test]
    fn levels_by_format() {
        use wgpu::TextureFormat as F;
        assert_eq!(OutputNode::levels(F::Rgba8Unorm), Some(255));
        assert_eq!(OutputNode::levels(F::Bgra8Unorm), Some(255));
        assert_eq!(OutputNode::levels(F::Rgb10a2Unorm), Some(1023));
        assert_eq!(OutputNode::levels(F::Rgba16Float), None);
    }
}
