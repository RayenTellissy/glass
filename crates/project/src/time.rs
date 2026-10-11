//! Exact project time.
//!
//! [`Time`] is an exact rational number of seconds, so a position is never rounded: a
//! video frame at 30000/1001 fps is exactly `1001/30000` s and an audio sample at 48 kHz is
//! exactly `1/48000` s. Items on video, subtitle and adjustment tracks start and end on
//! frame boundaries of their sequence's frame rate, items on audio tracks on sample
//! boundaries of its sample rate (see [`Time::is_multiple_of`]); edits that would break this
//! are rejected. Source times (inside a media file) may fall between frames, for example
//! under a speed change; the decoder picks the frame showing at that instant.
//!
//! `media` has its own `Rational`/`Timestamp` in a stream's time base; `project` may not
//! depend on `media` (docs/ARCHITECTURE.md, section 2), so the few types it needs live here.
//! [`Rational`] has the same shape and serde form as `media::Rational`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Errors from exact time arithmetic.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("time arithmetic overflowed")]
    Overflow,
    #[error("zero denominator")]
    ZeroDenominator,
    #[error("not a time: {0:?}")]
    Parse(String),
}

/// A fraction `num / den` for frame rates (frames per second) and speeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}

impl Rational {
    pub const ONE: Rational = Rational::new(1, 1);

    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// True for a usable rate: both parts positive.
    pub fn is_positive_rate(self) -> bool {
        self.num > 0 && self.den > 0
    }

    pub fn to_f64(self) -> Option<f64> {
        (self.den != 0).then(|| f64::from(self.num) / f64::from(self.den))
    }

    /// The duration of one frame at this rate, `den / num` seconds.
    pub fn frame_duration(self) -> Result<Time, TimeError> {
        Time::new(i64::from(self.den), i64::from(self.num))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// An exact number of seconds, `num / den`, always stored reduced with `den > 0`, so
/// structural equality is numeric equality.
///
/// Serialised as a string, `"1001/30000"`, or `"5"` for whole seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Time {
    num: i64,
    den: i64,
}

impl Time {
    pub const ZERO: Time = Time { num: 0, den: 1 };

    pub fn new(num: i64, den: i64) -> Result<Self, TimeError> {
        Self::from_i128(i128::from(num), i128::from(den))
    }

    /// Whole seconds.
    pub const fn seconds(s: i64) -> Self {
        Self { num: s, den: 1 }
    }

    /// `n` frames at `rate` frames per second.
    pub fn from_frames(n: i64, rate: Rational) -> Result<Self, TimeError> {
        Self::from_i128(i128::from(n) * i128::from(rate.den), i128::from(rate.num))
    }

    /// `n` samples at `sample_rate` Hz.
    pub fn from_samples(n: i64, sample_rate: u32) -> Result<Self, TimeError> {
        Self::new(n, i64::from(sample_rate))
    }

    fn from_i128(num: i128, den: i128) -> Result<Self, TimeError> {
        if den == 0 {
            return Err(TimeError::ZeroDenominator);
        }
        let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()).max(1) as i128;
        let (num, den) = (num / g, den / g);
        Ok(Self {
            num: i64::try_from(num).map_err(|_| TimeError::Overflow)?,
            den: i64::try_from(den).map_err(|_| TimeError::Overflow)?,
        })
    }

    pub fn num(self) -> i64 {
        self.num
    }

    pub fn den(self) -> i64 {
        self.den
    }

    /// Seconds as a float, for display and tolerances only.
    pub fn as_secs_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn is_zero(self) -> bool {
        self.num == 0
    }

    pub fn is_negative(self) -> bool {
        self.num < 0
    }

    pub fn is_positive(self) -> bool {
        self.num > 0
    }

    pub fn checked_add(self, other: Time) -> Result<Time, TimeError> {
        let (a, b) = (i128::from(self.den), i128::from(other.den));
        Self::from_i128(i128::from(self.num) * b + i128::from(other.num) * a, a * b)
    }

    pub fn checked_sub(self, other: Time) -> Result<Time, TimeError> {
        self.checked_add(-other)
    }

    /// `self * num / den`, exact.
    pub fn mul_ratio(self, num: i64, den: i64) -> Result<Time, TimeError> {
        Self::from_i128(
            i128::from(self.num) * i128::from(num),
            i128::from(self.den) * i128::from(den),
        )
    }

    pub fn mul_rational(self, r: Rational) -> Result<Time, TimeError> {
        self.mul_ratio(i64::from(r.num), i64::from(r.den))
    }

    /// `self * a / b` for three times, exact (used for linear interpolation).
    pub fn mul_div(self, a: Time, b: Time) -> Result<Time, TimeError> {
        if b.num == 0 {
            return Err(TimeError::ZeroDenominator);
        }
        // (sn/sd) * (an/ad) / (bn/bd) = sn*an*bd / (sd*ad*bn); reduce pairwise first.
        let n = [i128::from(self.num), i128::from(a.num), i128::from(b.den)];
        let d = [i128::from(self.den), i128::from(a.den), i128::from(b.num)];
        let num = n
            .iter()
            .try_fold(1i128, |acc, v| acc.checked_mul(*v))
            .ok_or(TimeError::Overflow)?;
        let den = d
            .iter()
            .try_fold(1i128, |acc, v| acc.checked_mul(*v))
            .ok_or(TimeError::Overflow)?;
        Self::from_i128(num, den)
    }

    pub fn abs(self) -> Time {
        Time {
            num: self.num.abs(),
            den: self.den,
        }
    }

    /// True when `self` is a whole number of `step`s (a frame or sample boundary).
    pub fn is_multiple_of(self, step: Time) -> bool {
        if step.num == 0 {
            return false;
        }
        // self / step = (sn * td) / (sd * tn) is an integer.
        let n = i128::from(self.num) * i128::from(step.den);
        let d = i128::from(self.den) * i128::from(step.num);
        n % d == 0
    }

    /// The whole number of `step`s nearest to this time, halves rounded away from zero
    /// (2.5 steps is 3, -2.5 is -3). `step` must be positive.
    pub fn round_to_multiple(self, step: Time) -> Result<i64, TimeError> {
        if !step.is_positive() {
            return Err(TimeError::ZeroDenominator);
        }
        // self / step = n / d with d > 0.
        let n = i128::from(self.num) * i128::from(step.den);
        let d = i128::from(self.den) * i128::from(step.num);
        let half_up = |n: i128| -> Option<i128> {
            let twice = d.checked_mul(2)?;
            Some(n.checked_mul(2)?.checked_add(d)?.div_euclid(twice))
        };
        let count = if n < 0 {
            half_up(-n).map(|c| -c)
        } else {
            half_up(n)
        };
        count
            .and_then(|c| i64::try_from(c).ok())
            .ok_or(TimeError::Overflow)
    }

    /// The number of whole `rate` frames at or before this time.
    pub fn frames_floor(self, rate: Rational) -> i64 {
        let n = i128::from(self.num) * i128::from(rate.num);
        let d = i128::from(self.den) * i128::from(rate.den);
        if d == 0 {
            return 0;
        }
        let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
        n.div_euclid(d).clamp(i64::MIN.into(), i64::MAX.into()) as i64
    }
}

impl std::ops::Neg for Time {
    type Output = Time;

    fn neg(self) -> Time {
        Time {
            num: -self.num,
            den: self.den,
        }
    }
}

impl Default for Time {
    fn default() -> Self {
        Self::ZERO
    }
}

impl Ord for Time {
    fn cmp(&self, other: &Self) -> Ordering {
        (i128::from(self.num) * i128::from(other.den))
            .cmp(&(i128::from(other.num) * i128::from(self.den)))
    }
}

impl PartialOrd for Time {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

impl FromStr for Time {
    type Err = TimeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parse = |v: &str| {
            v.trim()
                .parse::<i64>()
                .map_err(|_| TimeError::Parse(s.to_string()))
        };
        match s.split_once('/') {
            Some((n, d)) => Time::new(parse(n)?, parse(d)?),
            None => Ok(Time::seconds(parse(s)?)),
        }
    }
}

impl Serialize for Time {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A half-open span `[start, start + duration)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Time,
    pub duration: Time,
}

impl TimeRange {
    pub const fn new(start: Time, duration: Time) -> Self {
        Self { start, duration }
    }

    /// The range from `start` to `end`.
    pub fn between(start: Time, end: Time) -> Result<Self, TimeError> {
        Ok(Self::new(start, end.checked_sub(start)?))
    }

    pub fn end(self) -> Result<Time, TimeError> {
        self.start.checked_add(self.duration)
    }

    /// True when the two ranges share any instant (touching ends do not overlap).
    pub fn overlaps(self, other: TimeRange) -> Result<bool, TimeError> {
        Ok(self.start < other.end()? && other.start < self.end()?)
    }

    pub fn contains(self, t: Time) -> Result<bool, TimeError> {
        Ok(self.start <= t && t < self.end()?)
    }

    /// Both ends on whole multiples of `step`.
    pub fn is_aligned(self, step: Time) -> bool {
        self.start.is_multiple_of(step) && self.duration.is_multiple_of(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Time {
        s.parse().expect("time")
    }

    #[test]
    fn reduced_and_ordered() {
        assert_eq!(t("2/4"), t("1/2"));
        assert_eq!(t("-3/-6"), t("1/2"));
        assert_eq!(t("3/-6").to_string(), "-1/2");
        assert!(t("1001/30000") < t("1/29"));
        assert!(t("1001/30000") > t("1/30"));
        assert_eq!(Time::seconds(5).to_string(), "5");
        assert!(matches!("x".parse::<Time>(), Err(TimeError::Parse(_))));
        assert_eq!("1/0".parse::<Time>(), Err(TimeError::ZeroDenominator));
    }

    #[test]
    fn frames_and_samples_are_exact() {
        let ntsc = Rational::new(30000, 1001);
        let f = Time::from_frames(3, ntsc).expect("frames");
        assert_eq!(f, t("1001/10000"));
        assert_eq!(f.frames_floor(ntsc), 3);
        assert!(f.is_multiple_of(ntsc.frame_duration().expect("rate")));
        let s = Time::from_samples(1, 48_000).expect("samples");
        assert!(!s.is_multiple_of(ntsc.frame_duration().expect("rate")));
        // One frame at 30000/1001 is 1601.6 samples at 48 kHz; five frames are 8008.
        assert!(!f.is_multiple_of(s));
        let five = Time::from_frames(5, ntsc).expect("frames");
        assert!(five.is_multiple_of(s));
        assert_eq!(t("-1/60").frames_floor(Rational::new(30, 1)), -1);
    }

    #[test]
    fn rounding_to_a_multiple() {
        let frame = t("1/25");
        assert_eq!(t("2/25").round_to_multiple(frame), Ok(2));
        assert_eq!(t("5/50").round_to_multiple(frame), Ok(3));
        assert_eq!(t("49/1000").round_to_multiple(frame), Ok(1));
        assert_eq!(t("19/1000").round_to_multiple(frame), Ok(0));
        assert_eq!(t("-5/50").round_to_multiple(frame), Ok(-3));
        assert_eq!(t("-49/1000").round_to_multiple(frame), Ok(-1));
        assert_eq!(
            t("1").round_to_multiple(Time::ZERO),
            Err(TimeError::ZeroDenominator)
        );
        assert_eq!(
            t("1").round_to_multiple(t("-1")),
            Err(TimeError::ZeroDenominator)
        );
    }

    #[test]
    fn arithmetic() {
        assert_eq!(t("1/3").checked_add(t("1/6")), Ok(t("1/2")));
        assert_eq!(t("1/3").checked_sub(t("1/2")), Ok(t("-1/6")));
        assert_eq!(t("3/4").mul_ratio(2, 3), Ok(t("1/2")));
        assert_eq!(t("1").mul_div(t("1/2"), t("1/4")), Ok(t("2")));
        assert_eq!(
            Time::new(i64::MAX, 1)
                .expect("max")
                .checked_add(Time::seconds(1)),
            Err(TimeError::Overflow)
        );
    }

    #[test]
    fn ranges() {
        let a = TimeRange::new(t("0"), t("1"));
        let b = TimeRange::new(t("1"), t("1"));
        assert_eq!(a.overlaps(b), Ok(false));
        assert_eq!(a.overlaps(TimeRange::new(t("1/2"), t("1"))), Ok(true));
        assert!(a.is_aligned(t("1/25")));
        assert!(!TimeRange::new(t("1/48000"), t("1")).is_aligned(t("1/25")));
    }

    #[test]
    fn serde_as_string() {
        let json = serde_json::to_string(&TimeRange::new(t("1001/30000"), t("2"))).expect("ser");
        assert_eq!(json, r#"{"start":"1001/30000","duration":"2"}"#);
        let back: TimeRange = serde_json::from_str(&json).expect("de");
        assert_eq!(back.start, t("1001/30000"));
    }
}
