//! Automation lanes: a parameter's value over timeline frames, as breakpoints.
//!
//! A [`Lane`] is what the mixer evaluates at its control points (every
//! [`CONTROL_FRAMES`](super::CONTROL_FRAMES) frames of the timeline). Each point holds its
//! value from its frame to the next point, either as a straight line to the next value
//! ([`Shape::Linear`]) or flat ([`Shape::Hold`]); before the first point the lane holds the
//! first value and after the last one the last value, as project keyframe curves do. Curved
//! segments (a project curve's bezier keys) arrive here already flattened into short linear
//! pieces by the caller, which keeps the lane cheap to evaluate inside the callback.

/// How a lane moves from a point to the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    /// A straight line to the next point's value.
    #[default]
    Linear,
    /// This point's value until the next point.
    Hold,
}

/// One breakpoint of a [`Lane`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Timeline frame (sample) the point sits on.
    pub frame: i64,
    pub value: f64,
    /// How the value moves from this point to the next (unused on the last point).
    pub shape: Shape,
}

impl Point {
    pub fn linear(frame: i64, value: f64) -> Self {
        Self {
            frame,
            value,
            shape: Shape::Linear,
        }
    }

    pub fn hold(frame: i64, value: f64) -> Self {
        Self {
            frame,
            value,
            shape: Shape::Hold,
        }
    }
}

/// Why a lane was refused.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum LaneError {
    #[error("an automation lane needs at least one point")]
    Empty,
    #[error("automation points must be in strictly increasing frame order (frame {0})")]
    OutOfOrder(i64),
    #[error("automation value {0} is not finite")]
    NotFinite(f64),
}

/// A parameter's automation: breakpoints in strictly increasing frame order. See the
/// module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    points: Vec<Point>,
}

impl Lane {
    /// A lane through `points`, which must be non-empty, finite and strictly increasing in
    /// frame.
    pub fn new(points: Vec<Point>) -> Result<Self, LaneError> {
        if points.is_empty() {
            return Err(LaneError::Empty);
        }
        for (i, p) in points.iter().enumerate() {
            if !p.value.is_finite() {
                return Err(LaneError::NotFinite(p.value));
            }
            if i > 0 && points[i - 1].frame >= p.frame {
                return Err(LaneError::OutOfOrder(p.frame));
            }
        }
        Ok(Self { points })
    }

    /// A lane holding `value` everywhere.
    pub fn constant(value: f64) -> Result<Self, LaneError> {
        Self::new(vec![Point::hold(0, value)])
    }

    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// The value at timeline frame `frame`. Allocation-free.
    pub fn value_at(&self, frame: i64) -> f64 {
        let next = self.points.partition_point(|p| p.frame <= frame);
        if next == 0 {
            return self.points[0].value;
        }
        let a = &self.points[next - 1];
        match (a.shape, self.points.get(next)) {
            (Shape::Linear, Some(b)) if a.frame != frame => {
                let x = (frame - a.frame) as f64 / (b.frame - a.frame) as f64;
                a.value + (b.value - a.value) * x
            }
            _ => a.value,
        }
    }
}
