//! Phase 12's Done when, headless: a 16-track mix with buses, sends, automation and
//! crossfades plays without dropouts at a 256-frame buffer. The `glass-audio-decode` thread decodes and
//! resamples 16 files ahead of the playhead; this test's thread stands in for the device,
//! calling the mixing callback every 256 frames at real-time pace for three seconds,
//! counting underruns, allocations on the callback (a counting global allocator, so this
//! test lives alone in its binary) and the callback's time against its 5.3 ms budget.

mod common;
mod mix_common;

use std::time::{Duration, Instant};

use common::timing::{self, Laps, Stat, Stopwatch};
use engine::audio::OfflineOutput;
use engine::audio::mixer::{Layout, Mixer, MixerConfig, Route, SendSpec};
use engine::mix::{AudioPlayback, AudioPlaybackConfig, BusMix, MixSettings};
use engine::project::audio_transition::{self, EQUAL_POWER, LINEAR, S_CURVE};
use engine::project::params::ParamValue;
use engine::project::{Alignment, ClipParam, Curve, Interpolation, Key, KeyTarget, TransitionSpec};
use mix_common::{Place, Tone, build, samples, temp_dir};

/// The counting allocator, counting only on the thread that asks: the only unsafe code of
/// these tests, kept in an `ffi` module as the workspace rule asks.
#[allow(unsafe_code)]
mod ffi {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    thread_local! {
        /// This thread's allocations and frees while counting, `None` when not counting.
        pub static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
    }

    fn count() {
        let _ = COUNT.try_with(|n| {
            if let Some(c) = n.get() {
                n.set(Some(c + 1));
            }
        });
    }

    // SAFETY: forwards to the system allocator unchanged; only counts calls.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: same contract as the caller's.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            count();
            // SAFETY: same contract as the caller's.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count();
            // SAFETY: same contract as the caller's.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    pub fn counted<R>(f: impl FnOnce() -> R) -> (R, usize) {
        COUNT.with(|n| n.set(Some(0)));
        let result = f();
        let n = COUNT.with(|n| n.replace(None)).unwrap_or(0);
        (result, n)
    }
}

const TRACKS: usize = 16;
const BUFFER: usize = 256;
const RATE: u32 = 48_000;
const SECONDS: f64 = 3.0;

#[test]
fn sixteen_tracks_play_without_dropouts_at_a_256_frame_buffer() {
    let _serial = timing::serial();
    let dir = temp_dir("realtime");
    // Twelve files at the mix rate and four at 44.1 kHz, which the decode thread resamples.
    let files: Vec<_> = (0..TRACKS)
        .map(|t| {
            let tone = Tone {
                rate: if t % 4 == 3 { 44_100 } else { RATE },
                seconds: 5.0,
                channels: vec![
                    (110.0 * (t + 1) as f64, 0.05),
                    (97.0 * (t + 1) as f64, 0.05),
                ],
            };
            let path = dir.join(format!("tone{t}.wav"));
            tone.write(&path);
            path
        })
        .collect();
    // Tracks 1, 5, 9 and 13 hold the same file cut in two at 2 s, crossfaded below.
    let cut = |t: usize| t % 4 == 1;
    let places: Vec<Vec<Place>> = (0..TRACKS)
        .map(|t| {
            if cut(t) {
                vec![
                    Place {
                        media: t,
                        start: 0.0,
                        length: 2.0,
                        source_start: 0.25,
                    },
                    Place {
                        media: t,
                        start: 2.0,
                        length: 2.5,
                        source_start: 2.25,
                    },
                ]
            } else {
                vec![Place {
                    media: t,
                    start: 0.0,
                    length: 4.5,
                    source_start: 0.25,
                }]
            }
        })
        .collect();
    let built = build(&files, &places);
    let mut project = built.project.clone();
    // 0.1 s crossfades on the cut tracks (equal power, linear, S-curve, equal power) and
    // 0.1 s fades in from nothing on tracks 2 and 10. Unoptimised test builds decode at
    // little more than real time, so the extra readers of a crossfade are kept short
    // enough for the 170 ms feed to absorb.
    let curves = [EQUAL_POWER, LINEAR, S_CURVE];
    for t in (1..TRACKS).step_by(4) {
        let spec = audio_transition::crossfade_spec(
            Some(built.clips[t][0]),
            Some(built.clips[t][1]),
            samples(0.1),
            curves[t / 4 % 3],
        );
        project = project.add_transition(spec).unwrap().0;
    }
    for t in [2, 10] {
        let mut spec = TransitionSpec::new(None, Some(built.clips[t][0]), samples(0.1));
        spec.alignment = Alignment::Start;
        project = project.add_transition(spec).unwrap().0;
    }
    // Clip gain ramps on four clips.
    for t in (0..TRACKS).step_by(4) {
        let target = KeyTarget::Clip {
            item: built.clips[t][0],
            param: ClipParam::GainDb,
        };
        project = project.animate_param(target, samples(0.25)).unwrap();
        let target = KeyTarget::Clip {
            item: built.clips[t][0],
            param: ClipParam::GainDb,
        };
        project = project
            .set_key(target, samples(3.0), ParamValue::Float(-12.0))
            .unwrap();
    }
    // Fader automation (eased), pan, sends to a bus that feeds a second bus.
    let mut settings = MixSettings::default();
    for (t, &track) in built.tracks.iter().enumerate() {
        let mix = settings.track_mut(track);
        mix.pan = t as f64 / 8.0 - 1.0;
        if t % 3 == 0 {
            mix.fader_curve = Some(Curve {
                keys: vec![
                    Key {
                        interpolation: Interpolation::ease(1),
                        ..Key::new(samples(0.0), ParamValue::Float(-20.0))
                    },
                    Key::new(samples(2.0), ParamValue::Float(0.0)),
                ],
            });
        }
        if t % 2 == 0 {
            mix.sends.push(SendSpec {
                bus: 0,
                level_db: -9.0,
                pre_fader: t % 4 == 0,
            });
        }
        if t % 5 == 0 {
            mix.output = Route::Bus(1);
        }
    }
    settings.buses.push(BusMix {
        output: Route::Bus(1),
        fader_db: -3.0,
        ..BusMix::new(1)
    });
    settings.buses.push(BusMix {
        pan: 0.1,
        ..BusMix::new(2)
    });

    let (mixer, control) = Mixer::new(MixerConfig {
        sample_rate: RATE,
        layout: Layout::Stereo,
        commands: 64,
    });
    let status = control.status().clone();
    let (mut device, clock) = OfflineOutput::with_mixer(mixer);
    let config = AudioPlaybackConfig::default();
    let playback =
        AudioPlayback::start(project, built.sequence, settings, control, 0, config).unwrap();
    // Let the decode thread fill the feed before playing, as the app does on play.
    let waited = Instant::now();
    while playback.buffered_frames() < (config.ring_frames - config.chunk_frames) as i64 {
        assert!(
            waited.elapsed() < Duration::from_secs(20),
            "the feed did not fill"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let meters = playback.take_meters().expect("the first graph's meters");
    let mut out = vec![0.0f32; BUFFER * 2];
    playback.play();
    // The command reaches the callback through the decode thread; the callbacks before it
    // play silence and leave the playhead at 0.
    while !status.playing() {
        assert!(
            waited.elapsed() < Duration::from_secs(20),
            "play did not arrive"
        );
        device.render(&mut out);
        std::thread::sleep(Duration::from_millis(1));
    }

    let budget = Duration::from_secs_f64(BUFFER as f64 / f64::from(RATE));
    let callbacks = (SECONDS * f64::from(RATE) / BUFFER as f64) as usize;
    let mut laps = Laps::default();
    let mut allocations = 0;
    let mut energy = 0.0f64;
    // The callback that took the play command already advanced the playhead.
    let first = status.position();
    let started = Instant::now();
    for i in 0..callbacks {
        let due = started + budget * i as u32;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        let watch = Stopwatch::start();
        let ((), n) = ffi::counted(|| device.render(&mut out));
        laps.push(watch.stop());
        allocations += n;
        energy += out
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>();
    }
    let elapsed = started.elapsed();
    let stats = playback.stats();
    playback.shutdown();
    let _ = std::fs::remove_dir_all(&dir);

    let (wall, cpu) = (laps.wall(), laps.cpu());
    let budget_ms = budget.as_secs_f64() * 1000.0;
    println!(
        "16-track mix, {callbacks} callbacks of {BUFFER} frames in {:.2} s: {} underruns, \
         {} silent frames, {allocations} allocations; callback CPU mean {:.4} ms ({:.2}% of \
         the {budget_ms:.2} ms budget), p99 {:.4} ms, max {:.4} ms; wall {wall}; decode \
         thread rendered {} frames, {} silent clips",
        elapsed.as_secs_f64(),
        status.underruns(),
        status.silent_frames(),
        cpu.mean(),
        100.0 * cpu.mean() / budget_ms,
        cpu.p99(),
        cpu.max(),
        stats.rendered_frames,
        stats.silent_clips,
    );
    assert_eq!(stats.silent_clips, 0);
    assert!(energy > 0.0, "the mix was silent");
    assert!(meters.publications() > 0);
    assert_eq!(
        status.underruns(),
        0,
        "{} frames of dropout",
        status.silent_frames()
    );
    assert_eq!(clock.underruns(), 0);
    assert_eq!(allocations, 0, "the callback allocated or freed");
    assert_eq!(
        status.position() - first,
        (callbacks * BUFFER) as i64,
        "the playhead followed the callbacks"
    );
    laps.assert_within("mixing callback", Stat::P99, budget_ms);
}
