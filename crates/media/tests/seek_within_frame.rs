//! Seeks that land inside a frame's display interval: on every video fixture, in software
//! and, where the machine has a hardware decoder, in hardware, `seek_exact(t)` returns
//! the frame whose interval contains `t` (pts <= t < next pts), matching a linear decode's
//! checksum. Requests are frame starts, mid-frame times and the tick before the next
//! frame, in the first GOP and in later ones, made both from elsewhere in the file and
//! from the frame the reader has just returned (the same frame again, a later time
//! inside it, the next frame). Skips without fixtures.

mod common;

use media::{
    DecoderOptions, FrameReader, GopIndex, HwPreference, StreamSelect, Timestamp, VideoFrame,
};

/// BLAKE3 of the visible samples of every plane (CPU) or of the surface (hardware).
fn checksum(frame: &VideoFrame) -> [u8; 32] {
    if let Some(hw) = frame.hardware() {
        return hw.content_checksum().expect("surface readable");
    }
    let format = frame.format();
    let bytes = if format.bit_depth() > 8 { 2 } else { 1 };
    let mut hasher = blake3::Hasher::new();
    for (i, plane) in frame.planes().iter().enumerate() {
        let interleaved = if format.plane_count() == 2 && i == 1 {
            2
        } else {
            1
        };
        let row = plane.width as usize * bytes * interleaved;
        for line in plane.data.chunks(plane.stride).take(plane.height as usize) {
            hasher.update(&line[..row]);
        }
    }
    *hasher.finalize().as_bytes()
}

fn open(path: &std::path::Path, hw: HwPreference) -> Option<FrameReader> {
    FrameReader::open_with(path, StreamSelect::BestVideo, DecoderOptions::with_hw(hw)).ok()
}

/// Every frame of a linear decode: presentation time and checksum.
fn linear(path: &std::path::Path, hw: HwPreference) -> Vec<(Timestamp, [u8; 32])> {
    let reader = open(path, hw).expect("open");
    reader
        .map(|f| {
            let frame = f.expect("decode").into_video().expect("video");
            (frame.pts().expect("pts"), checksum(&frame))
        })
        .collect()
}

/// Frames to probe, each with two frames after it (so it and the next frame have a known
/// display interval): the first frame and the last frame of the first GOP (whose next
/// frame is the second GOP's keyframe), and, when there is more than one GOP, the middle
/// of the second GOP.
fn samples(frames: &[(Timestamp, [u8; 32])], keyframes: &[Timestamp]) -> Vec<usize> {
    let start_of = |kf: Timestamp| frames.partition_point(|(pts, _)| pts.cmp_time(kf).is_lt());
    let first_gop = keyframes.get(1).map_or(frames.len(), |&kf| start_of(kf));
    let mut picks = vec![0, first_gop.saturating_sub(1)];
    if let Some(&second) = keyframes.get(1) {
        let second = start_of(second);
        let end = keyframes.get(2).map_or(frames.len(), |&kf| start_of(kf));
        picks.push(second + (end - second) / 2);
    }
    let limit = frames.len().saturating_sub(3);
    let mut picks: Vec<usize> = picks.into_iter().map(|k| k.min(limit)).collect();
    picks.sort_unstable();
    picks.dedup();
    picks
}

struct Sweep<'a> {
    label: String,
    reader: FrameReader,
    frames: &'a [(Timestamp, [u8; 32])],
    /// Frames before this index belong to the first GOP.
    first_gop: usize,
    requests: usize,
    failures: Vec<String>,
}

impl Sweep<'_> {
    fn start(&self, k: usize) -> Timestamp {
        self.frames[k].0
    }

    fn mid(&self, k: usize) -> Timestamp {
        let (a, b) = (self.frames[k].0, self.frames[k + 1].0);
        Timestamp::new(a.value + (b.value - a.value) / 2, a.time_base)
    }

    /// One tick before the next frame starts: still frame `k` on screen.
    fn before_next(&self, k: usize) -> Timestamp {
        let next = self.frames[k + 1].0;
        Timestamp::new(next.value - 1, next.time_base)
    }

    /// `seek_exact(t)` must return frame `k`; a mismatch is recorded, not fatal, so one
    /// run lists every failing case.
    fn expect(&mut self, case: &str, t: Timestamp, k: usize) {
        self.requests += 1;
        let gop = if k < self.first_gop {
            "first GOP"
        } else {
            "later GOP"
        };
        let (pts, sum) = self.frames[k];
        let got = self
            .reader
            .seek_exact(t)
            .expect("seek")
            .map(|f| f.into_video().expect("video"));
        let got = got.as_ref().map(|f| (f.pts(), checksum(f)));
        if got != Some((Some(pts), sum)) {
            let shown = match got {
                Some((Some(p), s)) if p == pts && s != sum => {
                    format!("pts {} with a different checksum", p.value)
                }
                Some((Some(p), _)) => format!("frame at pts {} ({:.4} s)", p.value, p.seconds()),
                Some((None, _)) => "a frame without pts".to_string(),
                None => "no frame".to_string(),
            };
            self.failures.push(format!(
                "{}: frame {k} ({gop}), {case}: seek_exact({} = {:.4} s) expected pts {} ({:.4} s), got {shown}",
                self.label,
                t.value,
                t.seconds(),
                pts.value,
                pts.seconds(),
            ));
        }
    }

    /// The requests around frame `k`, in an order that leaves the reader on frame `k` or
    /// `k + 1` (with and without a frame decoded past it) before each within-frame request.
    fn around(&mut self, k: usize) {
        self.expect(
            "tick before the next frame, from elsewhere",
            self.before_next(k),
            k,
        );
        self.expect("mid-frame, earlier in the current frame", self.mid(k), k);
        self.expect(
            "frame start, earlier in the current frame",
            self.start(k),
            k,
        );
        self.expect("frame start again (same frame)", self.start(k), k);
        self.expect("mid-frame of the current frame", self.mid(k), k);
        self.expect(
            "tick before the next frame, current frame",
            self.before_next(k),
            k,
        );
        self.expect("the next frame", self.start(k + 1), k + 1);
        self.expect(
            "tick before the next frame, current frame after an exact hit",
            self.before_next(k + 1),
            k + 1,
        );
        self.expect("frame start, going back", self.start(k), k);
        let next = self
            .reader
            .next_frame()
            .expect("decode")
            .and_then(|f| f.pts());
        if next != Some(self.frames[k + 1].0) {
            self.failures.push(format!(
                "{}: frame {k}: next_frame after the seek gave {next:?}, expected frame {}",
                self.label,
                k + 1
            ));
            return;
        }
        self.expect(
            "mid-frame of the frame next_frame returned",
            self.mid(k + 1),
            k + 1,
        );
    }
}

/// Runs the sweep on one fixture; the failures, or `None` when it was skipped.
fn check_file(file: &str, hw: HwPreference) -> Option<(usize, Vec<String>)> {
    let path = common::fixture(file)?;
    let Some(reader) = open(&path, hw) else {
        eprintln!("{file}: no hardware decoder, skipped");
        return None;
    };
    if hw == HwPreference::Require && !reader.is_hardware() {
        eprintln!("{file}: decodes in software on this machine, skipped");
        return None;
    }
    let frames = linear(&path, hw);
    assert!(frames.len() >= 3, "{file}: {} frames", frames.len());
    let (gop, _) = GopIndex::build(&path, StreamSelect::BestVideo).expect("gop index");
    let keyframes: Vec<Timestamp> = (0..gop.keyframes.len())
        .filter_map(|i| gop.keyframe_pts(i))
        .collect();
    let first_gop = keyframes.get(1).map_or(frames.len(), |&kf| {
        frames.partition_point(|(pts, _)| pts.cmp_time(kf).is_lt())
    });
    let mut sweep = Sweep {
        label: format!("{file} ({hw:?})"),
        reader,
        frames: &frames,
        first_gop,
        requests: 0,
        failures: Vec::new(),
    };
    // Last sample first, so the first request of each sample comes from another GOP or
    // from later in the file.
    for k in samples(&frames, &keyframes).into_iter().rev() {
        sweep.around(k);
    }
    eprintln!(
        "{}: {} requests, {} wrong",
        sweep.label,
        sweep.requests,
        sweep.failures.len()
    );
    Some((sweep.requests, sweep.failures))
}

fn video_fixtures() -> Vec<String> {
    let Some(files) = common::manifest() else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|e| e["kind"] == "video")
        .filter_map(|e| e["file"].as_str().map(str::to_string))
        .collect()
}

fn assert_all_exact(results: impl IntoIterator<Item = (usize, Vec<String>)>) {
    let mut requests = 0;
    let mut failures = Vec::new();
    for (n, f) in results {
        requests += n;
        failures.extend(f);
    }
    assert!(
        failures.is_empty(),
        "{} of {requests} seeks returned the wrong frame:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn software_seeks_within_a_frame_return_that_frame() {
    assert_all_exact(
        video_fixtures()
            .iter()
            .filter_map(|file| check_file(file, HwPreference::Off)),
    );
}

#[cfg(target_os = "macos")]
#[test]
fn hardware_seeks_within_a_frame_return_that_frame() {
    assert_all_exact(video_fixtures().iter().filter_map(|file| {
        let codec = media::probe(common::generated_dir().join(file))
            .ok()
            .and_then(|i| {
                i.streams
                    .iter()
                    .find(|s| s.kind == media::MediaKind::Video)
                    .map(|s| s.codec.name)
            });
        codec
            .is_some_and(media::hardware_decoder_available)
            .then(|| check_file(file, HwPreference::Require))
            .flatten()
    }));
}
