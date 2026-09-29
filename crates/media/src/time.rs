//! Exact media time: a rational number of seconds per tick and a tick count in that base.
//!
//! Every timestamp in `media` is a [`Timestamp`] in its stream's time base, so nothing is
//! rounded until a caller asks for seconds or rescales to another base.

use std::cmp::Ordering;
use std::fmt;

/// A fraction `num / den`. Used for time bases (seconds per tick), frame rates (frames per
/// second) and aspect ratios. A zero denominator means "unknown", as in FFmpeg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    /// FFmpeg's internal time base, one microsecond (`AV_TIME_BASE_Q`).
    pub const MICROS: Rational = Rational::new(1, 1_000_000);

    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// True when both parts are non-zero, so the value is a usable rate or base.
    pub fn is_valid(self) -> bool {
        self.num != 0 && self.den != 0
    }

    /// The value as a float, or `None` when the denominator is zero.
    pub fn to_f64(self) -> Option<f64> {
        (self.den != 0).then(|| f64::from(self.num) / f64::from(self.den))
    }

    /// `den / num`, for example a frame rate turned into a frame duration.
    pub fn invert(self) -> Self {
        Self::new(self.den, self.num)
    }

    /// The same value with the smallest terms and a positive denominator.
    pub fn reduced(self) -> Self {
        if self.den == 0 {
            return self;
        }
        let g = gcd(
            i64::from(self.num).unsigned_abs(),
            i64::from(self.den).unsigned_abs(),
        )
        .max(1);
        let sign = if self.den < 0 { -1 } else { 1 };
        // Dividing by the gcd never grows a value, so the casts stay in range.
        Self::new(
            (i64::from(self.num) * sign / g as i64) as i32,
            (i64::from(self.den) * sign / g as i64) as i32,
        )
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl From<ffmpeg_next::Rational> for Rational {
    fn from(r: ffmpeg_next::Rational) -> Self {
        Self::new(r.numerator(), r.denominator())
    }
}

impl From<Rational> for ffmpeg_next::Rational {
    fn from(r: Rational) -> Self {
        ffmpeg_next::Rational::new(r.num, r.den)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A point in time: `value` ticks of `time_base` seconds each.
///
/// Equality is structural (same value and base); use [`Timestamp::cmp_time`] to compare
/// timestamps in different bases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timestamp {
    pub value: i64,
    pub time_base: Rational,
}

impl Timestamp {
    pub const fn new(value: i64, time_base: Rational) -> Self {
        Self { value, time_base }
    }

    /// The nearest tick of `time_base` to `seconds`.
    pub fn from_seconds(seconds: f64, time_base: Rational) -> Self {
        let per_tick = time_base.to_f64().unwrap_or(0.0);
        let value = if per_tick > 0.0 {
            (seconds / per_tick).round() as i64
        } else {
            0
        };
        Self::new(value, time_base)
    }

    /// Seconds as a float, for display and tolerances. Zero when the base is unknown.
    pub fn seconds(self) -> f64 {
        self.value as f64 * self.time_base.to_f64().unwrap_or(0.0)
    }

    /// The same instant in another base, rounded to the nearest tick (halves away from
    /// zero, like FFmpeg's `av_rescale_q`). Saturates instead of overflowing.
    pub fn rescale(self, to: Rational) -> Self {
        if self.time_base == to {
            return self;
        }
        let num = i128::from(self.time_base.num) * i128::from(to.den);
        let den = i128::from(self.time_base.den) * i128::from(to.num);
        if den == 0 {
            return Self::new(0, to);
        }
        let product = i128::from(self.value) * num;
        let (product, den) = if den < 0 {
            (-product, -den)
        } else {
            (product, den)
        };
        let half = den / 2;
        let rounded = if product >= 0 {
            (product + half) / den
        } else {
            (product - half) / den
        };
        Self::new(rounded.clamp(i64::MIN.into(), i64::MAX.into()) as i64, to)
    }

    /// Exact ordering of two instants, whatever their bases.
    pub fn cmp_time(self, other: Timestamp) -> Ordering {
        let (n1, d1) = positive_den(self.time_base);
        let (n2, d2) = positive_den(other.time_base);
        // v1 * n1 / d1  vs  v2 * n2 / d2, cross-multiplied; i64 * i32 * i32 fits in i128.
        (i128::from(self.value) * n1 * d2).cmp(&(i128::from(other.value) * n2 * d1))
    }
}

fn positive_den(r: Rational) -> (i128, i128) {
    if r.den < 0 {
        (-i128::from(r.num), -i128::from(r.den))
    } else {
        (i128::from(r.num), i128::from(r.den))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} x {} ({:.6} s)",
            self.value,
            self.time_base,
            self.seconds()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_basics() {
        assert_eq!(
            Rational::new(30000, 1001)
                .to_f64()
                .map(|v| (v * 1000.0).round()),
            Some(29970.0)
        );
        assert_eq!(Rational::new(1, 0).to_f64(), None);
        assert!(!Rational::new(0, 1).is_valid());
        assert_eq!(Rational::new(60, -90).reduced(), Rational::new(-2, 3));
        assert_eq!(Rational::new(25, 1).invert(), Rational::new(1, 25));
    }

    #[test]
    fn rescale_rounds_to_nearest() {
        let t = Timestamp::new(1001, Rational::new(1, 30000));
        assert_eq!(t.rescale(Rational::new(1, 1000)).value, 33);
        assert_eq!(
            Timestamp::new(-1001, Rational::new(1, 30000))
                .rescale(Rational::new(1, 1000))
                .value,
            -33
        );
        assert_eq!(
            Timestamp::new(3, Rational::new(1, 2))
                .rescale(Rational::new(1, 1))
                .value,
            2
        );
        assert_eq!(t.rescale(t.time_base), t);
    }

    #[test]
    fn rescale_saturates() {
        let t = Timestamp::new(i64::MAX, Rational::new(1, 1));
        assert_eq!(t.rescale(Rational::MICROS).value, i64::MAX);
    }

    #[test]
    fn compare_across_bases() {
        let a = Timestamp::new(1001, Rational::new(1, 30000));
        let b = Timestamp::new(33, Rational::new(1, 1000));
        assert_eq!(a.cmp_time(b), Ordering::Greater);
        assert_eq!(b.cmp_time(a), Ordering::Less);
        let c = Timestamp::new(2, Rational::new(1, 60));
        let d = Timestamp::new(1, Rational::new(1, 30));
        assert_eq!(c.cmp_time(d), Ordering::Equal);
    }

    #[test]
    fn seconds_round_trip() {
        let t = Timestamp::from_seconds(1.5, Rational::new(1, 90000));
        assert_eq!(t.value, 135000);
        assert!((t.seconds() - 1.5).abs() < 1e-12);
    }
}
