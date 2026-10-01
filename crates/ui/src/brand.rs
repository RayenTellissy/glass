//! The Glass mark and app tile drawn with the painter, from the Brand page's geometry
//! (`assets/icons/source/`, embedded by `assets::brand`), so the About dialog shows the
//! real icon at any scale without raster files.
//!
//! Rules from the Brand page and `assets/icons/README.md`: the mark keeps 4 slices at
//! 32 points and larger and drops to the 3-slice small mark below; the graphite tile's
//! glow is drawn only on tiles larger than 64 points; the mark only ever changes size.

use std::sync::{Arc, OnceLock};

use egui::epaint::{Mesh, TessellationOptions, Tessellator};
use egui::{CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};

use crate::theme::brand;

/// The mark's size relative to its tile, from the Brand page's About window (54 of 84).
pub const MARK_IN_TILE: f32 = 54.0 / 84.0;
/// The tile's corner radius relative to its size (19 of 84, Apple's 22% grid).
const TILE_RADIUS: f32 = 19.0 / 84.0;
/// The tile's drop shadow on the 84-point board: 8 down, 16 blur.
const SHADOW_OFFSET: f32 = 8.0 / 84.0;
const SHADOW_BLUR: f32 = 16.0 / 84.0;
/// Below this drawn size (points) the mark uses 3 slices.
const SMALL_BELOW: f32 = 32.0;
/// The SVG grid the mark is drawn on.
const GRID: f32 = 200.0;

/// One slice of the mark: its outline on the 200-unit grid and its white's opacity.
struct Slice {
    points: Vec<Pos2>,
    alpha: u8,
}

fn slices(small: bool) -> &'static [Slice] {
    static FULL: OnceLock<Vec<Slice>> = OnceLock::new();
    static SMALL: OnceLock<Vec<Slice>> = OnceLock::new();
    if small {
        SMALL.get_or_init(|| parse_svg(assets::brand::MARK_SMALL_SVG))
    } else {
        FULL.get_or_init(|| parse_svg(assets::brand::MARK_SVG))
    }
}

/// Draws the mark filling the square `rect`.
pub fn mark(painter: &Painter, rect: Rect) {
    let small = rect.width() < SMALL_BELOW;
    let k = rect.width() / GRID;
    for slice in slices(small) {
        let points = slice
            .points
            .iter()
            .map(|p| rect.min + p.to_vec2() * k)
            .collect();
        painter.add(Shape::convex_polygon(
            points,
            brand::MARK.gamma_multiply_u8(slice.alpha),
            Stroke::NONE,
        ));
    }
}

/// Draws the graphite app tile with the mark floating in it, filling the square `rect`.
pub fn app_tile(painter: &Painter, rect: Rect) {
    let size = rect.width();
    let corner = CornerRadius::same((size * TILE_RADIUS).round().clamp(0.0, 255.0) as u8);
    painter.add(
        egui::epaint::Shadow {
            offset: [0, (size * SHADOW_OFFSET).round() as i8],
            blur: (size * SHADOW_BLUR).round() as u8,
            spread: 0,
            color: brand::TILE_SHADOW,
        }
        .as_shape(rect, corner),
    );
    vertical_gradient(painter, rect, corner, brand::TILE_TOP, brand::TILE_BOTTOM);
    // Inset 1 px edge ring and top highlight.
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0, brand::TILE_EDGE),
        StrokeKind::Inside,
    );
    let inset = f32::from(corner.nw);
    painter.line_segment(
        [
            pos2(rect.min.x + inset, rect.min.y + 0.5),
            pos2(rect.max.x - inset, rect.min.y + 0.5),
        ],
        Stroke::new(1.0, brand::TILE_HIGHLIGHT),
    );
    let mark_size = (size * MARK_IN_TILE).round();
    mark(
        painter,
        Rect::from_center_size(rect.center(), Vec2::splat(mark_size)),
    );
}

/// A rounded rectangle filled top to bottom from `top` to `bottom`, anti-aliased: the
/// filled shape is tessellated as usual and each vertex recoloured by its height,
/// keeping the edge feathering's coverage.
fn vertical_gradient(
    painter: &Painter,
    rect: Rect,
    corner: CornerRadius,
    top: egui::Color32,
    bottom: egui::Color32,
) {
    let ctx = painter.ctx();
    let mut tessellator = Tessellator::new(
        ctx.pixels_per_point(),
        TessellationOptions::default(),
        ctx.fonts(|f| f.font_image_size()),
        Vec::new(),
    );
    let mut mesh = Mesh::default();
    // Opaque, so each vertex's alpha is its anti-aliasing coverage.
    tessellator.tessellate_shape(Shape::rect_filled(rect, corner, top.to_opaque()), &mut mesh);
    for v in &mut mesh.vertices {
        let t = ((v.pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
        let coverage = v.color.a();
        v.color = top.lerp_to_gamma(bottom, t).gamma_multiply_u8(coverage);
    }
    painter.add(Shape::Mesh(Arc::new(mesh)));
}

/// Reads the `<path d=".." fill="#FFFFFFAA">` elements of a mark SVG: absolute `M`, `L`,
/// `A` and `Z` commands, arcs flattened to points.
fn parse_svg(svg: &str) -> Vec<Slice> {
    let mut out = Vec::new();
    for element in svg.split("<path").skip(1) {
        let attr = |name: &str| {
            let start = element.find(&format!("{name}=\""))? + name.len() + 2;
            let len = element[start..].find('"')?;
            Some(&element[start..start + len])
        };
        let (Some(d), Some(fill)) = (attr("d"), attr("fill")) else {
            continue;
        };
        let alpha = fill
            .strip_prefix('#')
            .and_then(|hex| hex.get(6..8))
            .and_then(|a| u8::from_str_radix(a, 16).ok())
            .unwrap_or(255);
        out.push(Slice {
            points: path_points(d),
            alpha,
        });
    }
    out
}

fn path_points(d: &str) -> Vec<Pos2> {
    let mut tokens = Vec::new();
    let mut number = String::new();
    for ch in d.chars() {
        if ch.is_ascii_alphabetic() {
            if !number.is_empty() {
                tokens.push(std::mem::take(&mut number));
            }
            tokens.push(ch.to_string());
        } else if ch.is_whitespace() || ch == ',' {
            if !number.is_empty() {
                tokens.push(std::mem::take(&mut number));
            }
        } else {
            number.push(ch);
        }
    }
    if !number.is_empty() {
        tokens.push(number);
    }
    let mut points: Vec<Pos2> = Vec::new();
    let mut i = 0;
    let num = |i: &mut usize| -> f32 {
        let v = tokens.get(*i).and_then(|t| t.parse().ok()).unwrap_or(0.0);
        *i += 1;
        v
    };
    let mut command = 'M';
    while i < tokens.len() {
        if let Some(c) = tokens[i].chars().next().filter(char::is_ascii_alphabetic) {
            command = c;
            i += 1;
            if command == 'Z' || command == 'z' {
                continue;
            }
        }
        match command {
            'M' | 'L' => {
                let p = pos2(num(&mut i), num(&mut i));
                points.push(p);
            }
            'A' => {
                let (rx, ry, rotation) = (num(&mut i), num(&mut i), num(&mut i));
                let (large, sweep) = (num(&mut i) != 0.0, num(&mut i) != 0.0);
                let end = pos2(num(&mut i), num(&mut i));
                let start = points.last().copied().unwrap_or(end);
                points.extend(arc_points(start, end, vec2(rx, ry), rotation, large, sweep));
            }
            _ => i += 1,
        }
    }
    points
}

/// Points along an SVG elliptical arc from `start` to `end` (excluding `start`), by the
/// endpoint-to-centre conversion of SVG 1.1 appendix F.6.5.
fn arc_points(
    start: Pos2,
    end: Pos2,
    r: Vec2,
    rotation_deg: f32,
    large: bool,
    sweep: bool,
) -> Vec<Pos2> {
    const STEPS: usize = 8;
    let (sin, cos) = rotation_deg.to_radians().sin_cos();
    let half = (start - end) / 2.0;
    let p = vec2(cos * half.x + sin * half.y, -sin * half.x + cos * half.y);
    let (mut rx, mut ry) = (r.x.abs(), r.y.abs());
    if rx == 0.0 || ry == 0.0 {
        return vec![end];
    }
    let lambda = (p.x * p.x) / (rx * rx) + (p.y * p.y) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * p.y * p.y - ry * ry * p.x * p.x;
    let den = rx * rx * p.y * p.y + ry * ry * p.x * p.x;
    let mut k = (num / den).max(0.0).sqrt();
    if large == sweep {
        k = -k;
    }
    let c1 = vec2(k * rx * p.y / ry, -k * ry * p.x / rx);
    let mid = (start.to_vec2() + end.to_vec2()) / 2.0;
    let center = pos2(
        cos * c1.x - sin * c1.y + mid.x,
        sin * c1.x + cos * c1.y + mid.y,
    );
    let angle = |v: Vec2| v.y.atan2(v.x);
    let theta1 = angle(vec2((p.x - c1.x) / rx, (p.y - c1.y) / ry));
    let mut delta = angle(vec2((-p.x - c1.x) / rx, (-p.y - c1.y) / ry)) - theta1;
    use std::f32::consts::TAU;
    if sweep && delta < 0.0 {
        delta += TAU;
    } else if !sweep && delta > 0.0 {
        delta -= TAU;
    }
    (1..=STEPS)
        .map(|s| {
            if s == STEPS {
                return end;
            }
            let a = theta1 + delta * s as f32 / STEPS as f32;
            let (x, y) = (rx * a.cos(), ry * a.sin());
            pos2(cos * x - sin * y + center.x, sin * x + cos * y + center.y)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marks_parse_into_their_slices() {
        // 9 paths in the 4-slice mark (face plus 4 side and 4 top slices), 7 in the small.
        assert_eq!(slices(false).len(), 9);
        assert_eq!(slices(true).len(), 7);
        for slice in slices(false).iter().chain(slices(true)) {
            assert!(slice.points.len() > 8);
            assert!(
                slice
                    .points
                    .iter()
                    .all(|p| (0.0..=GRID).contains(&p.x) && (0.0..=GRID).contains(&p.y))
            );
        }
        // The front slice of the top is solid white, the side slices translucent.
        assert!(slices(false).iter().any(|s| s.alpha == 255));
        assert!(slices(false).iter().any(|s| s.alpha == 0x33));
    }

    #[test]
    fn a_quarter_arc_lands_on_its_circle() {
        let points = arc_points(
            pos2(10.0, 0.0),
            pos2(0.0, 10.0),
            vec2(10.0, 10.0),
            0.0,
            false,
            true,
        );
        assert_eq!(*points.last().unwrap(), pos2(0.0, 10.0));
        for p in points {
            assert!((p.to_vec2().length() - 10.0).abs() < 1e-3, "{p:?}");
        }
    }
}
