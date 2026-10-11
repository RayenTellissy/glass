//! [`CachedFrames`]: which timeline frames the frame cache holds, for the scrubber's
//! immediate answer, and the nearest-frame rule.

use std::collections::BTreeSet;
use std::sync::{Arc, PoisonError, RwLock};

/// Which way the pointer last moved along the timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// No move yet (the first request), or only repeats of one frame.
    #[default]
    Still,
    Forward,
    Backward,
}

impl Direction {
    /// The direction from `from` to `to`; `Still` when they are equal.
    pub fn of_move(from: i64, to: i64) -> Self {
        match to.cmp(&from) {
            std::cmp::Ordering::Greater => Self::Forward,
            std::cmp::Ordering::Less => Self::Backward,
            std::cmp::Ordering::Equal => Self::Still,
        }
    }

    /// +1 forward, -1 backward, 0 still.
    pub fn sign(self) -> i64 {
        match self {
            Self::Still => 0,
            Self::Forward => 1,
            Self::Backward => -1,
        }
    }
}

/// The timeline frames resident in the rendered-frame cache at the current preview
/// variant, shared between whoever fills or evicts the cache and the scrubber. The
/// scrubber's UI side only ever `try_read`s it, so a writer holding the lock costs the
/// UI one fallback answer, never a wait. Cheap to clone (one `Arc`).
#[derive(Clone, Debug, Default)]
pub struct CachedFrames {
    frames: Arc<RwLock<BTreeSet<i64>>>,
}

/// The index was being written; try again or keep what is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

impl CachedFrames {
    pub fn new() -> Self {
        Self::default()
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeSet<i64>> {
        self.frames.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeSet<i64>> {
        self.frames.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn insert(&self, frame: i64) {
        self.write().insert(frame);
    }

    pub fn remove(&self, frame: i64) {
        self.write().remove(&frame);
    }

    /// Keeps the frames `keep` returns true for (an invalidated range, a new variant).
    pub fn retain(&self, keep: impl FnMut(&i64) -> bool) {
        self.write().retain(keep);
    }

    pub fn clear(&self) {
        self.write().clear();
    }

    pub fn contains(&self, frame: i64) -> bool {
        self.read().contains(&frame)
    }

    /// The end of the run of cached frames from `frame` on, at most `limit`: `frame`
    /// itself when it is not cached.
    pub fn run_end(&self, frame: i64, limit: i64) -> i64 {
        let frames = self.read();
        let mut end = frame;
        for &f in frames.range(frame..limit) {
            if f != end {
                break;
            }
            end += 1;
        }
        end
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// [`nearest`] over the index without waiting for a writer.
    pub fn try_nearest(
        &self,
        frame: i64,
        direction: Direction,
        ahead_bias: f64,
    ) -> Result<Option<i64>, Busy> {
        match self.frames.try_read() {
            Ok(frames) => Ok(nearest(&frames, frame, direction, ahead_bias)),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Ok(nearest(
                &poisoned.into_inner(),
                frame,
                direction,
                ahead_bias,
            )),
            Err(std::sync::TryLockError::WouldBlock) => Err(Busy),
        }
    }
}

/// The cached frame to show for a request at `frame`: `frame` itself when cached,
/// otherwise the closest cached frame on either side, where the frame on the side the
/// pointer moves toward wins unless it is more than `ahead_bias` times as far as the one
/// behind (by the time it is on screen the pointer has moved toward it). With no
/// direction the closer frame wins, the earlier one on a tie.
pub fn nearest(
    frames: &BTreeSet<i64>,
    frame: i64,
    direction: Direction,
    ahead_bias: f64,
) -> Option<i64> {
    let before = frames.range(..=frame).next_back().copied();
    if before == Some(frame) {
        return before;
    }
    let after = frames.range(frame..).next().copied();
    let (ahead, behind) = match direction {
        Direction::Backward => (before, after),
        Direction::Forward | Direction::Still => (after, before),
    };
    match (ahead, behind) {
        (None, other) | (other, None) => other,
        (Some(ahead), Some(behind)) => {
            let ahead_distance = (ahead - frame).unsigned_abs() as f64;
            let behind_distance = (behind - frame).unsigned_abs() as f64;
            let ahead_wins = match direction {
                // `ahead` is the later frame here; the earlier one wins a tie.
                Direction::Still => ahead_distance < behind_distance,
                _ => ahead_distance <= behind_distance * ahead_bias.max(0.0),
            };
            Some(if ahead_wins { ahead } else { behind })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(frames: &[i64]) -> BTreeSet<i64> {
        frames.iter().copied().collect()
    }

    #[test]
    fn an_exact_hit_wins_whatever_the_direction() {
        let frames = set(&[10, 20, 30]);
        for direction in [Direction::Still, Direction::Forward, Direction::Backward] {
            assert_eq!(nearest(&frames, 20, direction, 2.0), Some(20));
        }
    }

    #[test]
    fn with_no_direction_the_closer_frame_wins_and_ties_go_earlier() {
        let frames = set(&[10, 20]);
        assert_eq!(nearest(&frames, 13, Direction::Still, 2.0), Some(10));
        assert_eq!(nearest(&frames, 17, Direction::Still, 2.0), Some(20));
        assert_eq!(nearest(&frames, 15, Direction::Still, 2.0), Some(10));
    }

    #[test]
    fn the_side_the_pointer_moves_toward_wins_within_the_bias() {
        let frames = set(&[10, 20]);
        // 4 behind, 6 ahead: ahead wins moving forward (6 <= 2 x 4), behind otherwise.
        assert_eq!(nearest(&frames, 14, Direction::Forward, 2.0), Some(20));
        assert_eq!(nearest(&frames, 14, Direction::Backward, 2.0), Some(10));
        // 2 behind, 8 ahead: more than twice as far, so the closer frame wins.
        assert_eq!(nearest(&frames, 12, Direction::Forward, 2.0), Some(10));
        // Moving backward from 16: 10 is 6 ahead, 20 is 4 behind.
        assert_eq!(nearest(&frames, 16, Direction::Backward, 2.0), Some(10));
        assert_eq!(nearest(&frames, 18, Direction::Backward, 2.0), Some(20));
        // A bias of 1 is plain nearest with ties to the moving side.
        assert_eq!(nearest(&frames, 15, Direction::Forward, 1.0), Some(20));
        assert_eq!(nearest(&frames, 15, Direction::Backward, 1.0), Some(10));
    }

    #[test]
    fn one_sided_and_empty_sets() {
        assert_eq!(nearest(&set(&[]), 5, Direction::Forward, 2.0), None);
        assert_eq!(nearest(&set(&[3]), 50, Direction::Forward, 2.0), Some(3));
        assert_eq!(nearest(&set(&[90]), 5, Direction::Backward, 2.0), Some(90));
    }

    #[test]
    fn try_nearest_reports_a_writer_instead_of_waiting() {
        let cached = CachedFrames::new();
        cached.insert(7);
        let guard = cached.frames.write().unwrap();
        assert_eq!(cached.try_nearest(7, Direction::Still, 2.0), Err(Busy));
        drop(guard);
        assert_eq!(cached.try_nearest(9, Direction::Still, 2.0), Ok(Some(7)));
    }

    #[test]
    fn run_end_stops_at_the_first_gap_or_the_limit() {
        let cached = CachedFrames::new();
        for f in [3, 4, 5, 7] {
            cached.insert(f);
        }
        assert_eq!(cached.run_end(3, 100), 6);
        assert_eq!(cached.run_end(4, 5), 5);
        assert_eq!(cached.run_end(6, 100), 6);
        assert_eq!(cached.run_end(7, 100), 8);
    }
}
