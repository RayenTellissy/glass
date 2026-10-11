//! Layer masks and their CPU reference.
//!
//! A [`Mask`] is one or more [`Shape`]s in layer pixels. Its coverage is the union (the
//! larger value per pixel) of its shapes' interiors, each rasterised as exact box-filtered
//! coverage by [`crate::shape::CoverageNode`] with the shape's own fill rule (non-zero
//! when it has no fill; strokes are not part of a mask). Then, per mask:
//!
//! - **Feather** softens the edge over `feather` layer pixels on each side: a separable
//!   Gaussian of standard deviation `feather / 3`, truncated at `feather` (rounded up to
//!   whole pixels) and renormalised. It is symmetric, so a straight edge stays at 0.5
//!   where it was and the ramp runs from about 0 at `feather` outside it to about 1 at
//!   `feather` inside (0.0013 and 0.9987 before truncation). Pixels beyond the layer
//!   repeat its border. 0 keeps the hard edge.
//! - **Invert** turns coverage `c` into `1 - c`, after the feather.
//! - **Mode** combines the mask with the masks above it in the list: [`MaskMode::Union`]
//!   takes the larger value, [`MaskMode::Intersection`] the smaller. The first mask
//!   starts the result, so its mode does not matter.
//!
//! The result multiplies the layer's premultiplied linear-light colour before the layer
//! is transformed and blended ([`add_to_graph`] puts a [`MaskNode`] between the layer's
//! source and its [`crate::nodes::CompositeNode`]), as opacity does.

use crate::graph::{Graph, Port};
use crate::nodes::MaskNode;
use crate::renderer::Renderer;
use crate::shape::{CoverageNode, FillRule, Shape};

/// The largest feather, in layer pixels: the blur reads about one texel per pixel of
/// feather on each pass, so this bounds a pass at a few hundred fetches.
pub const MAX_FEATHER: f32 = 250.0;

/// How a mask combines with the masks above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MaskMode {
    /// The larger of the two: either mask shows the layer.
    #[default]
    Union,
    /// The smaller of the two: only where both show it.
    Intersection,
}

impl MaskMode {
    /// `acc` (the masks so far) combined with `value`.
    pub fn combine(self, acc: f64, value: f64) -> f64 {
        match self {
            Self::Union => acc.max(value),
            Self::Intersection => acc.min(value),
        }
    }

    /// The shader's `combine` code.
    pub(crate) fn code(self) -> u32 {
        match self {
            Self::Union => 1,
            Self::Intersection => 2,
        }
    }
}

/// One mask of a layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    /// Shapes in layer pixels (each shape's transform maps it into the layer), unioned.
    pub shapes: Vec<Shape>,
    /// Softening radius in layer pixels each side of the edge; 0 is a hard edge. Clamped
    /// to 0..[`MAX_FEATHER`].
    pub feather: f32,
    pub invert: bool,
    pub mode: MaskMode,
}

impl Mask {
    /// A hard-edged union mask of `shapes`.
    pub fn new(shapes: Vec<Shape>) -> Self {
        Self {
            shapes,
            feather: 0.0,
            invert: false,
            mode: MaskMode::Union,
        }
    }

    pub fn feathered(mut self, feather: f32) -> Self {
        self.feather = feather;
        self
    }

    pub fn inverted(mut self) -> Self {
        self.invert = true;
        self
    }

    pub fn with_mode(mut self, mode: MaskMode) -> Self {
        self.mode = mode;
        self
    }

    /// The feather used: clamped to 0..[`MAX_FEATHER`], NaN as 0.
    pub fn feather_radius(&self) -> f32 {
        if self.feather.is_nan() {
            return 0.0;
        }
        self.feather.clamp(0.0, MAX_FEATHER)
    }
}

/// Half-width of the feather's kernel in whole pixels (0 for no blur).
pub fn kernel_radius(feather: f32) -> u32 {
    feather.max(0.0).ceil() as u32
}

/// `1 / (2 sigma^2)` of the feather's Gaussian, sigma being a third of the feather.
pub fn falloff(feather: f32) -> f32 {
    let sigma = feather.max(1e-6) / 3.0;
    1.0 / (2.0 * sigma * sigma)
}

/// The feather's normalised kernel, offsets `-r..=r` with `r` = [`kernel_radius`]; `[1]`
/// for no feather.
pub fn kernel(feather: f32) -> Vec<f64> {
    let r = kernel_radius(feather) as i32;
    let falloff = f64::from(falloff(feather));
    let weights: Vec<f64> = (-r..=r)
        .map(|k| (-f64::from(k * k) * falloff).exp())
        .collect();
    let total: f64 = weights.iter().sum();
    weights.into_iter().map(|w| w / total).collect()
}

/// `coverage` (rows of `size`) feathered: the separable Gaussian along rows, then
/// columns, clamping to the edge.
pub fn feather(coverage: &[f64], size: (u32, u32), feather: f32) -> Vec<f64> {
    let kernel = kernel(feather);
    if kernel.len() == 1 {
        return coverage.to_vec();
    }
    let (w, h) = (size.0 as i64, size.1 as i64);
    let r = (kernel.len() / 2) as i64;
    let pass = |src: &[f64], horizontal: bool| -> Vec<f64> {
        let mut out = vec![0.0; src.len()];
        for y in 0..h {
            for x in 0..w {
                out[(y * w + x) as usize] = kernel
                    .iter()
                    .zip(-r..=r)
                    .map(|(k, d)| {
                        let (sx, sy) = if horizontal {
                            ((x + d).clamp(0, w - 1), y)
                        } else {
                            (x, (y + d).clamp(0, h - 1))
                        };
                        k * src[(sy * w + sx) as usize]
                    })
                    .sum();
            }
        }
        out
    };
    pass(&pass(coverage, true), false)
}

/// One mask's value at a pixel from its feathered coverage: clamped, then inverted if
/// asked.
pub fn mask_value(mask: &Mask, feathered: f64) -> f64 {
    let v = feathered.clamp(0.0, 1.0);
    if mask.invert { 1.0 - v } else { v }
}

/// The masks' values at one pixel combined in list order: each mask's mode against the
/// result of those above it, the first starting it. No masks is 1 (the layer unchanged).
pub fn combine(values: impl IntoIterator<Item = (MaskMode, f64)>) -> f64 {
    let mut values = values.into_iter();
    let Some((_, first)) = values.next() else {
        return 1.0;
    };
    values.fold(first, |acc, (mode, v)| mode.combine(acc, v))
}

/// The CPU reference of a layer's combined mask: `unions[i]` is mask `i`'s shapes'
/// coverage unioned (rows of `size`), feathered, inverted and combined per pixel.
pub fn reference(masks: &[Mask], unions: &[Vec<f64>], size: (u32, u32)) -> Vec<f64> {
    let feathered: Vec<Vec<f64>> = masks
        .iter()
        .zip(unions)
        .map(|(m, c)| feather(c, size, m.feather_radius()))
        .collect();
    (0..(size.0 * size.1) as usize)
        .map(|i| {
            combine(
                masks
                    .iter()
                    .zip(&feathered)
                    .map(|(m, f)| (m.mode, mask_value(m, f[i]))),
            )
        })
        .collect()
}

/// The fill rule a mask rasterises `shape` with: its fill's, else non-zero.
pub fn fill_rule(shape: &Shape) -> FillRule {
    shape.fill.as_ref().map_or(FillRule::NonZero, |f| f.rule)
}

/// Masks `layer` (a working-space texture of `size` pixels) with `masks`: one
/// [`CoverageNode`] per shape (one with nothing for a mask without shapes) and a
/// [`MaskNode`] reading the layer and them. Returns the masked layer, or `layer` itself
/// when there are no masks.
pub fn add_to_graph(
    graph: &mut Graph<'_>,
    renderer: &Renderer,
    layer: Port,
    size: (u32, u32),
    masks: &[Mask],
) -> Port {
    if masks.is_empty() {
        return layer;
    }
    let mut inputs = vec![layer];
    for mask in masks {
        if mask.shapes.is_empty() {
            let empty = CoverageNode::new(renderer, size, &Default::default());
            inputs.push(graph.add(empty, &[]).out(0));
        }
        for shape in &mask.shapes {
            let outline = shape.outline_of_fill(fill_rule(shape));
            let node = CoverageNode::new(renderer, size, &outline);
            inputs.push(graph.add(node, &[]).out(0));
        }
    }
    graph.add(MaskNode::new(size, masks), &inputs).out(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{Geometry, Paint, Rect};

    fn mask(mode: MaskMode) -> Mask {
        Mask::new(Vec::new()).with_mode(mode)
    }

    #[test]
    fn union_takes_the_larger_and_intersection_the_smaller() {
        assert_eq!(MaskMode::Union.combine(0.25, 0.75), 0.75);
        assert_eq!(MaskMode::Union.combine(0.75, 0.25), 0.75);
        assert_eq!(MaskMode::Intersection.combine(0.25, 0.75), 0.25);
        assert_eq!(MaskMode::Intersection.combine(0.75, 0.25), 0.25);
    }

    #[test]
    fn masks_combine_in_list_order_from_the_first() {
        use MaskMode::{Intersection as I, Union as U};
        assert_eq!(combine([]), 1.0, "no masks leave the layer alone");
        assert_eq!(combine([(I, 0.3)]), 0.3, "the first mask's mode is moot");
        assert_eq!(combine([(U, 0.3)]), 0.3);
        // (0.2 U 0.9) I 0.5 = 0.5, but (0.2 I 0.5) U 0.9 = 0.9: order matters.
        assert_eq!(combine([(U, 0.2), (U, 0.9), (I, 0.5)]), 0.5);
        assert_eq!(combine([(U, 0.2), (I, 0.5), (U, 0.9)]), 0.9);
        assert_eq!(combine([(U, 0.2), (I, 0.5), (I, 0.1)]), 0.1);
        // Two disjoint shapes: union shows both, intersection neither.
        for (a, b) in [(1.0, 0.0), (0.0, 1.0), (0.0, 0.0)] {
            assert_eq!(combine([(U, a), (U, b)]), a.max(b));
            assert_eq!(combine([(U, a), (I, b)]), 0.0);
        }
    }

    #[test]
    fn invert_applies_after_clamping() {
        let m = mask(MaskMode::Union).inverted();
        assert_eq!(mask_value(&m, 0.25), 0.75);
        assert_eq!(mask_value(&m, 1.2), 0.0);
        assert_eq!(mask_value(&mask(MaskMode::Union), -0.1), 0.0);
        // An inverted mask intersected with another cuts a hole into it.
        let values = [
            (MaskMode::Union, 1.0),
            (MaskMode::Intersection, mask_value(&m, 1.0)),
        ];
        assert_eq!(combine(values), 0.0);
    }

    #[test]
    fn the_kernel_is_normalised_symmetric_and_ends_at_the_feather() {
        assert_eq!(kernel(0.0), vec![1.0]);
        assert_eq!(kernel(-3.0), vec![1.0]);
        for f in [0.5, 1.0, 2.5, 8.0, 40.0] {
            let k = kernel(f);
            assert_eq!(k.len(), 2 * kernel_radius(f) as usize + 1);
            assert!((k.iter().sum::<f64>() - 1.0).abs() < 1e-12);
            for i in 0..k.len() / 2 {
                assert_eq!(k[i], k[k.len() - 1 - i]);
            }
            // At the feather the Gaussian is exp(-4.5) of its peak (three sigma).
            if f.fract() == 0.0 {
                let ratio = k[0] / k[k.len() / 2];
                assert!((ratio - (-4.5f64).exp()).abs() < 1e-7, "{f}: {ratio}");
            }
        }
    }

    #[test]
    fn a_feathered_edge_stays_put_and_ramps_over_the_radius() {
        // A step at x = 50 on a 100 x 1 row: 1 left of it, 0 right.
        let size = (100, 1);
        let step: Vec<f64> = (0..100).map(|x| if x < 50 { 1.0 } else { 0.0 }).collect();
        assert_eq!(feather(&step, size, 0.0), step, "0 is a hard edge");
        let f = 10.0;
        let soft = feather(&step, size, f);
        // Symmetric about the edge: pixels mirrored across it sum to 1.
        for d in 0..20 {
            assert!((soft[49 - d] + soft[50 + d] - 1.0).abs() < 1e-12);
        }
        // Monotonic, about 0 and 1 at the radius, untouched beyond it.
        assert!(soft.windows(2).all(|w| w[0] >= w[1]));
        assert!(soft[50 + 9] > 0.0 && soft[50 + 9] < 0.002, "{}", soft[59]);
        assert!(soft[49 - 9] < 1.0 && soft[49 - 9] > 0.998, "{}", soft[40]);
        assert_eq!(soft[50 + 10], 0.0);
        assert!((soft[49 - 10] - 1.0).abs() < 1e-12);
        // The border repeats: the left end stays 1 and the right end 0.
        assert!((soft[0] - 1.0).abs() < 1e-12);
        assert_eq!(soft[99], 0.0);
    }

    #[test]
    fn the_reference_feathers_inverts_and_combines_each_mask() {
        let size = (8, 1);
        let a: Vec<f64> = vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let b: Vec<f64> = vec![0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0];
        let masks = [mask(MaskMode::Union), mask(MaskMode::Intersection)];
        let both = reference(&masks, &[a.clone(), b.clone()], size);
        assert_eq!(both, vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        let masks = [mask(MaskMode::Union), mask(MaskMode::Union)];
        let either = reference(&masks, &[a.clone(), b.clone()], size);
        assert_eq!(either, vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
        let masks = [mask(MaskMode::Union).inverted()];
        let outside = reference(&masks, &[a], size);
        assert_eq!(outside, vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn feather_is_clamped_and_shapes_mask_with_their_fill_rule() {
        assert_eq!(
            mask(MaskMode::Union).feathered(1e6).feather_radius(),
            MAX_FEATHER
        );
        assert_eq!(mask(MaskMode::Union).feathered(-2.0).feather_radius(), 0.0);
        assert_eq!(
            mask(MaskMode::Union).feathered(f32::NAN).feather_radius(),
            0.0
        );
        let rect = Shape::new(Geometry::Rect {
            rect: Rect::new(0.0, 0.0, 4.0, 4.0),
            radius: 0.0,
        });
        assert_eq!(fill_rule(&rect), FillRule::NonZero);
        let even_odd = rect.filled_with(Paint::Solid([1.0; 4]), FillRule::EvenOdd);
        assert_eq!(fill_rule(&even_odd), FillRule::EvenOdd);
    }
}
