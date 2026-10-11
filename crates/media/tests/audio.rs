//! `media::AudioReader`: every fixture with audio at 48 and 44.1 kHz (sample counts, seeks
//! against a linear read), sine frequency and amplitude after resampling, the channel
//! rules on a 5.1 fixture, and A/V alignment on the sync-marker fixtures. Skips when
//! `tests/fixtures/generate.sh` has not been run.

mod common;

use media::{AudioOptions, AudioReader, ChannelLayout, FrameReader, StreamSelect};

const RATES: [u32; 2] = [48_000, 44_100];

fn options(rate: u32, layout: ChannelLayout) -> AudioOptions {
    AudioOptions {
        sample_rate: rate,
        layout,
        ..AudioOptions::default()
    }
}

/// Reads the whole stream in chunks of `chunk` frames, planar.
fn read_all(reader: &mut AudioReader, chunk: usize) -> Vec<Vec<f32>> {
    let channels = reader.channels();
    let mut out = vec![Vec::new(); channels];
    let mut buffers = vec![vec![0.0f32; chunk]; channels];
    loop {
        let mut slices: Vec<&mut [f32]> = buffers.iter_mut().map(|b| b.as_mut_slice()).collect();
        let n = reader.read(&mut slices).expect("read");
        if n == 0 {
            break;
        }
        for (o, b) in out.iter_mut().zip(&buffers) {
            o.extend_from_slice(&b[..n]);
        }
    }
    out
}

/// The native stream: sample rate, index of the first sample at or after time 0, and
/// one past the last, from the decoded frames' pts.
fn native_extent(path: &std::path::Path) -> (u32, i64, i64) {
    let mut reader = FrameReader::open(path, StreamSelect::BestAudio).expect("open");
    let mut rate = 0;
    let mut first: Option<i64> = None;
    let mut end = 0i64;
    while let Some(frame) = reader.next_frame().expect("decode") {
        let Some(audio) = frame.into_audio() else {
            continue;
        };
        rate = audio.sample_rate();
        let pts = audio.pts().expect("pts");
        let start = (pts.seconds() * f64::from(rate)).round() as i64;
        let start = first.map_or(start, |_| end);
        first.get_or_insert(start);
        end = start + audio.samples() as i64;
    }
    (rate, first.unwrap_or(0).max(0), end)
}

fn audio_fixtures() -> Vec<(String, serde_json::Value)> {
    common::manifest()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e["audio"].is_object())
        .map(|e| (e["file"].as_str().expect("file").to_string(), e))
        .collect()
}

#[test]
fn every_fixture_decodes_at_both_rates() {
    for (file, entry) in audio_fixtures() {
        let path = common::generated_dir().join(&file);
        let (in_rate, first, end) = native_extent(&path);
        assert_eq!(
            Some(u64::from(in_rate)),
            entry["audio"]["sample_rate"].as_u64(),
            "{file}"
        );
        for rate in RATES {
            let mut reader =
                AudioReader::open(&path, options(rate, ChannelLayout::Stereo)).expect("open");
            let scale = |i: i64| i as f64 * f64::from(rate) / f64::from(in_rate);
            let expected_first = scale(first).ceil() as i64;
            let expected = scale(end).round() as i64 - expected_first;
            assert_eq!(
                reader.first_sample() as i64,
                expected_first,
                "{file} at {rate}"
            );
            let out = read_all(&mut reader, 1000);
            let got = out[0].len() as i64;
            eprintln!(
                "{file}: {in_rate} Hz -> {rate} Hz: {got} samples (expected {expected}), delay {} trimmed",
                reader.resampler_delay()
            );
            assert!(
                (got - expected).abs() <= 1,
                "{file} at {rate}: {got} samples, expected {expected}"
            );
            if entry["audio"]["codec"]
                .as_str()
                .is_some_and(|c| c.starts_with("pcm"))
                && let Some(duration) = entry["duration"].as_f64()
            {
                assert_eq!(
                    got,
                    (duration * f64::from(rate)).round() as i64,
                    "{file}: PCM"
                );
            }
            assert!(out.iter().all(|c| c.iter().all(|v| v.is_finite())));

            // The read size does not change what comes out.
            let mut again =
                AudioReader::open(&path, options(rate, ChannelLayout::Stereo)).expect("open");
            let odd = read_all(&mut again, 777);
            assert_eq!(odd[0].len(), out[0].len(), "{file}");
            assert!(
                odd[0].iter().zip(&out[0]).all(|(a, b)| a == b),
                "{file}: chunking changed the samples"
            );
        }
    }
}

#[test]
fn seeks_are_sample_accurate() {
    for (file, entry) in audio_fixtures() {
        let path = common::generated_dir().join(&file);
        for rate in RATES {
            let mut reader =
                AudioReader::open(&path, options(rate, ChannelLayout::Stereo)).expect("open");
            let first = reader.first_sample();
            let linear = read_all(&mut reader, 4096);
            let len = linear[0].len() as u64;
            let mut worst = 0.0f32;
            let mut worst_at = (0, 0);
            let mut state = 0x2545_f491_4f6c_dd1du64;
            let mut targets: Vec<u64> = (0..12)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    first + state % len
                })
                .collect();
            targets.extend([first, first + len - 10, first + 1]);
            for target in targets {
                reader.seek_to_sample(target).expect("seek");
                assert_eq!(reader.position(), target, "{file}");
                let mut a = vec![0.0f32; 2048];
                let mut b = vec![0.0f32; 2048];
                let n = reader.read(&mut [&mut a, &mut b]).expect("read");
                let at = (target - first) as usize;
                assert_eq!(
                    n,
                    2048.min(linear[0].len() - at),
                    "{file}: frames after seek"
                );
                // Channel 0 only: the right channel of the stereo fixtures is pink noise,
                // which AAC codes with perceptual noise substitution, and the decoder draws
                // that noise from a generator whose state a seek resets.
                let _ = &b;
                for i in 0..n {
                    let d = (a[i] - linear[0][at + i]).abs();
                    if d > worst {
                        worst = d;
                        worst_at = (target, i);
                    }
                }
            }
            eprintln!(
                "{file} at {rate} Hz: seeks, worst difference from a linear read {worst:.2e} (target {}, sample {})",
                worst_at.0, worst_at.1
            );
            // PCM repeats exactly (up to the resampler's float rounding). Lossy decoders
            // converge after the pre-roll rather than repeating bit for bit: AAC's noise
            // substitution and Opus's decoder state depend on where decoding started.
            let pcm = entry["audio"]["codec"]
                .as_str()
                .is_some_and(|c| c.starts_with("pcm"));
            let tolerance = if pcm { 1e-5 } else { 2e-3 };
            assert!(
                worst <= tolerance,
                "{file} at {rate}: seek differs by {worst}"
            );
        }
    }
}

/// Frequency from interpolated upward zero crossings and amplitude from the RMS.
fn sine_measure(samples: &[f32], rate: u32) -> (f64, f64) {
    let mut crossings = Vec::new();
    for i in 1..samples.len() {
        let (a, b) = (f64::from(samples[i - 1]), f64::from(samples[i]));
        if a < 0.0 && b >= 0.0 {
            crossings.push((i - 1) as f64 + a / (a - b));
        }
    }
    let span = crossings.last().expect("crossings") - crossings[0];
    let frequency = (crossings.len() - 1) as f64 / span * f64::from(rate);
    let rms =
        (samples.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / samples.len() as f64).sqrt();
    (frequency, rms * std::f64::consts::SQRT_2)
}

#[test]
fn sines_keep_frequency_and_amplitude() {
    for (file, entry) in audio_fixtures() {
        let Some(hz) = entry["sine_hz"].as_f64() else {
            continue;
        };
        let amplitude = entry["sine_amplitude"].as_f64().expect("amplitude");
        let channel = entry["sine_channel"].as_u64().unwrap_or(0) as usize;
        let path = common::generated_dir().join(&file);
        let measure = |rate: u32| {
            let mut reader =
                AudioReader::open(&path, options(rate, ChannelLayout::Stereo)).expect("open");
            let out = read_all(&mut reader, 1024);
            // The middle second, away from fades and codec edges.
            let r = rate as usize;
            (
                sine_measure(&out[channel][r / 2..r / 2 + r], rate),
                reader.source_rate(),
            )
        };
        let ((_, native), source_rate) = measure(48_000);
        let ((_, native), _) = if source_rate == 48_000 {
            ((0.0, native), source_rate)
        } else {
            measure(source_rate)
        };
        // Lossy encoders change the level a little (LAME: -0.26 dB here).
        let lossy = !file.ends_with(".wav");
        let tolerance = if lossy { 0.05 } else { 0.001 };
        assert!(
            (native - amplitude).abs() < amplitude * tolerance,
            "{file}: native amplitude {native}"
        );
        for rate in RATES {
            let ((f, a), _) = measure(rate);
            eprintln!(
                "{file} at {rate} Hz: {f:.4} Hz (expected {hz}), amplitude {a:.6} (source {native:.6}, {:+.4} dB)",
                20.0 * (a / native).log10()
            );
            assert!((f - hz).abs() < hz * 1e-4, "{file} at {rate}: {f} Hz");
            assert!(
                (a / native - 1.0).abs() < 0.002,
                "{file} at {rate}: amplitude {a}"
            );
        }
    }
}

#[test]
fn channel_rules_on_a_51_source() {
    let Some(path) = common::fixture("audio_51_48k.wav") else {
        return;
    };
    let [fl, fr, fc, lfe, sl, sr] = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
    let k = std::f32::consts::FRAC_1_SQRT_2;
    let left = fl + k * fc + k * sl;
    let right = fr + k * fc + k * sr;
    let cases: [(ChannelLayout, Vec<f32>); 3] = [
        (ChannelLayout::Stereo, vec![left, right]),
        (ChannelLayout::Mono, vec![(left + right) / 2.0]),
        (ChannelLayout::Surround51, vec![fl, fr, fc, lfe, sl, sr]),
    ];
    for (layout, expected) in cases {
        for rate in RATES {
            let mut reader = AudioReader::open(&path, options(rate, layout)).expect("open");
            let out = read_all(&mut reader, 512);
            for (c, want) in expected.iter().enumerate() {
                let v = out[c][out[c].len() / 2];
                assert!(
                    (v - want).abs() < 1e-3,
                    "{} at {rate}: channel {c} is {v}, expected {want}",
                    layout.name()
                );
            }
        }
    }
    // Mono sources play on both stereo channels at unity.
    let Some(mp3) = common::fixture("audio_44k1_mono.mp3") else {
        return;
    };
    let mut reader = AudioReader::open(&mp3, options(48_000, ChannelLayout::Stereo)).expect("open");
    let out = read_all(&mut reader, 1024);
    assert!(out[0] == out[1], "mono is copied to both channels");
}

/// The Hann-windowed 2 kHz burst of the sync fixtures, sampled at `rate`.
fn burst(rate: u32) -> Vec<f32> {
    let n = (0.005 * f64::from(rate)).round() as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let window = (std::f64::consts::PI * t / 0.005).sin().powi(2);
            (0.8 * (2.0 * std::f64::consts::PI * 2000.0 * t).sin() * window) as f32
        })
        .collect()
}

/// Presentation time of the white flash frame.
fn flash_time(path: &std::path::Path) -> f64 {
    let mut reader = FrameReader::open(path, StreamSelect::BestVideo).expect("open video");
    while let Some(frame) = reader.next_frame().expect("decode") {
        let video = frame.into_video().expect("video");
        let luma = video.plane(0).expect("luma");
        let row = &luma.data[..luma.width as usize];
        let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / row.len() as f64;
        if mean > 128.0 {
            return video.pts().expect("pts").seconds();
        }
    }
    panic!("{}: no flash frame", path.display());
}

#[test]
fn audio_lines_up_with_video() {
    for (file, entry) in audio_fixtures() {
        let Some(marker) = entry["sync_marker_s"].as_f64() else {
            continue;
        };
        let path = common::generated_dir().join(&file);
        let flash = flash_time(&path);
        assert!((flash - marker).abs() < 1e-6, "{file}: flash at {flash}");
        for rate in RATES {
            let mut reader =
                AudioReader::open(&path, options(rate, ChannelLayout::Stereo)).expect("open");
            let first = reader.first_sample() as usize;
            let out = read_all(&mut reader, 1024);
            let template = burst(rate);
            let r = rate as usize;
            // Cross-correlate around the marker, +-100 ms.
            let (lo, hi) = (r * 9 / 10, r * 11 / 10);
            let mut best = (f64::MIN, 0usize);
            for at in lo..hi {
                let Some(window) = at
                    .checked_sub(first)
                    .and_then(|i| out[0].get(i..i + template.len()))
                else {
                    continue;
                };
                let score: f64 = window
                    .iter()
                    .zip(&template)
                    .map(|(a, b)| f64::from(*a) * f64::from(*b))
                    .sum();
                if score > best.0 {
                    best = (score, at);
                }
            }
            let onset = best.1 as f64 / f64::from(rate);
            let offset_ms = (onset - flash) * 1000.0;
            eprintln!(
                "{file} at {rate} Hz: audio marker {onset:.6} s, video {flash:.6} s, offset {offset_ms:+.3} ms"
            );
            assert!(
                offset_ms.abs() <= 1.0,
                "{file} at {rate}: A/V offset {offset_ms} ms"
            );
        }
    }
}
