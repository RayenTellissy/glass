use std::sync::Arc;

use crate::colour::{self, Mat3, WorkingSpace};
use crate::graph::{Node, NodeContext, TextureDesc};
use crate::lut::{Domain, Lut, LutSpace};
use crate::renderer::{Effect, Filter, Uniform};

/// Width of the shaper texture's rows: a 1D table longer than this wraps onto more rows.
const SHAPER_ROW: usize = 4096;

/// A [`Lut`] uploaded for [`LutNode`]: the 3D table as an `Rgba16Float` 3D texture (red
/// along x, green along y, blue along z, as the file orders it) and the 1D shaper as an
/// `Rgba16Float` 2D texture, one entry per texel in rows of 4096. f16 holds table values
/// within 2^-11 relative, below a 10-bit code. Upload once per LUT and share it.
#[derive(Debug)]
pub struct GpuLut {
    cube: wgpu::TextureView,
    shaper: wgpu::TextureView,
    cube_size: u32,
    shaper_size: u32,
    cube_domain: Domain,
    shaper_domain: Domain,
}

fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 31 {
        return sign | 0x7c00;
    }
    // Shift the 24-bit significand (with the implicit 1) down to 11 bits, or further for
    // subnormals, rounding to nearest even.
    let (shift, base) = if e <= 0 {
        if e < -10 {
            return sign;
        }
        (14 - e, 0u32)
    } else {
        (13, (e as u32) << 10)
    };
    let full = man | 0x80_0000;
    let mut half = full >> shift;
    let rest = full & ((1 << shift) - 1);
    let midpoint = 1 << (shift - 1);
    if rest > midpoint || (rest == midpoint && half & 1 == 1) {
        half += 1;
    }
    // For normals `half` carries the implicit bit at 1 << 10; adding the exponent field
    // counts it once (and a round-up into the next binade lands there correctly).
    let value = if e <= 0 {
        half
    } else {
        base + half - (1 << 10)
    };
    sign | value as u16
}

fn texels(table: &[[f32; 3]], pad_to: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(pad_to * 8);
    for i in 0..pad_to {
        let [r, g, b] = table.get(i).copied().unwrap_or_default();
        for v in [r, g, b, 1.0] {
            data.extend_from_slice(&f32_to_f16(v).to_le_bytes());
        }
    }
    data
}

impl GpuLut {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, lut: &Lut) -> Self {
        let create = |label, size: wgpu::Extent3d, dimension, data: &[u8]| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                texture.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.width * 8),
                    rows_per_image: Some(size.height),
                },
                size,
            );
            texture.create_view(&Default::default())
        };
        // A table that is absent still needs a texture bound: one texel, skipped by the
        // shader since its size is 0.
        let (cube_size, cube_domain, cube_data) = match &lut.cube {
            Some(c) => (c.size, c.domain, texels(&c.table, c.table.len())),
            None => (0, Domain::UNIT, texels(&[], 1)),
        };
        let side = cube_size.max(1) as u32;
        let cube = create(
            "lut cube",
            wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: side,
            },
            wgpu::TextureDimension::D3,
            &cube_data,
        );
        let (shaper_size, shaper_domain, table) = match &lut.shaper {
            Some(s) => (s.size, s.domain, s.table.as_slice()),
            None => (0, Domain::UNIT, &[][..]),
        };
        let width = shaper_size.clamp(1, SHAPER_ROW);
        let rows = shaper_size.div_ceil(SHAPER_ROW).max(1);
        let shaper = create(
            "lut shaper",
            wgpu::Extent3d {
                width: width as u32,
                height: rows as u32,
                depth_or_array_layers: 1,
            },
            wgpu::TextureDimension::D2,
            &texels(table, width * rows),
        );
        Self {
            cube,
            shaper,
            cube_size: cube_size as u32,
            shaper_size: shaper_size as u32,
            cube_domain,
            shaper_domain,
        }
    }

    /// Entries per side of the 3D table, 0 without one.
    pub fn cube_size(&self) -> u32 {
        self.cube_size
    }

    /// Entries in the 1D shaper, 0 without one.
    pub fn shaper_size(&self) -> u32 {
        self.shaper_size
    }
}

/// Applies a LUT in the working space: alpha divided out, the working space converted
/// into the LUT's declared input space ([`LutSpace`]: gamut, then transfer), the 1D
/// shaper and tetrahedral 3D lookup, back to the working space, mixed with the input by
/// `intensity` and alpha multiplied back in. Intensity 0 passes the input through
/// unchanged. The CPU reference is [`crate::lut::apply_in_working_space`].
#[derive(Clone, Debug)]
pub struct LutNode {
    lut: Arc<GpuLut>,
    space: LutSpace,
    intensity: f32,
    into: Mat3,
    back: Mat3,
}

impl LutNode {
    pub fn new(lut: Arc<GpuLut>, working: WorkingSpace, space: LutSpace, intensity: f32) -> Self {
        let (into, back) = match space.primaries() {
            Some(p) => (
                colour::gamut_matrix(working.primaries, p),
                colour::gamut_matrix(p, working.primaries),
            ),
            None => (colour::IDENTITY, colour::IDENTITY),
        };
        Self {
            lut,
            space,
            intensity: intensity.clamp(0.0, 1.0),
            into,
            back,
        }
    }
}

fn domain_rows(d: &Domain) -> [[f32; 4]; 2] {
    // Minimum and 1 / (max - min), so the shader multiplies.
    [
        [d.min[0] as f32, d.min[1] as f32, d.min[2] as f32, 0.0],
        std::array::from_fn(|c| {
            if c < 3 {
                (1.0 / (d.max[c] - d.min[c])) as f32
            } else {
                0.0
            }
        }),
    ]
}

impl Node for LutNode {
    fn label(&self) -> &str {
        "lut"
    }

    fn input_count(&self) -> usize {
        1
    }

    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc> {
        vec![inputs[0].with_format(TextureDesc::WORKING_FORMAT)]
    }

    fn params(&self) -> Vec<(&'static str, String)> {
        vec![
            ("space", self.space.id().to_owned()),
            ("cube", self.lut.cube_size.to_string()),
            ("shaper", self.lut.shaper_size.to_string()),
            ("intensity", format!("{:.3}", self.intensity)),
        ]
    }

    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    ) {
        let rows = colour::uniform_rows(&self.into)
            .into_iter()
            .chain(colour::uniform_rows(&self.back))
            .chain(domain_rows(&self.lut.shaper_domain))
            .chain(domain_rows(&self.lut.cube_domain));
        let uniform = rows
            .fold(Uniform::new(), Uniform::vec4)
            .u32(self.space.code())
            .u32(self.lut.shaper_size)
            .u32(self.lut.cube_size)
            .f32(self.intensity);
        cx.draw(
            &Effect {
                label: "lut",
                shader: "lut",
                entry: "fs",
                uniform: uniform.bytes(),
                inputs: &[inputs[0], &self.lut.shaper, &self.lut.cube],
                filter: Filter::Nearest,
            },
            outputs[0],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f16_to_f32(h: u16) -> f32 {
        crate::readback::f16_to_f32(h)
    }

    #[test]
    fn f16_conversion_rounds_to_nearest_even() {
        for v in [
            0.0f32,
            1.0,
            -2.5,
            0.1,
            1.0 / 3.0,
            65504.0,
            6.1e-5,
            3.0e-7,
            0.999_9,
        ] {
            let back = f16_to_f32(f32_to_f16(v));
            let tol = (v.abs() * 2f32.powi(-11)).max(2f32.powi(-25));
            assert!((back - v).abs() <= tol, "{v} -> {back}");
        }
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(-2.0), 0xc000);
        assert_eq!(f32_to_f16(65504.0), 0x7bff);
        assert_eq!(f32_to_f16(1e6), 0x7c00);
        // 1 + 2^-11 is half way between 1 and the next f16: ties to even (1).
        assert_eq!(f32_to_f16(1.0 + 2f32.powi(-11)), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 3.0 * 2f32.powi(-11)), 0x3c02);
        // Just under 2 rounds up into the next binade.
        assert_eq!(f32_to_f16(2.0 - 2f32.powi(-13)), 0x4000);
        // Smallest subnormal.
        assert_eq!(f32_to_f16(2f32.powi(-24)), 0x0001);
        for i in 0..=64u16 {
            let v = f32::from(i) / 64.0;
            assert_eq!(f16_to_f32(f32_to_f16(v)), v, "grid values are exact");
        }
    }
}
