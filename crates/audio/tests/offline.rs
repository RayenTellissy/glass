//! The offline backend: manual clock timing, the ring's audio reaching the output,
//! underruns.

use audio::{OfflineOutput, OutputConfig};

fn config(ring_frames: usize) -> OutputConfig {
    OutputConfig {
        sample_rate: Some(48_000),
        channels: 2,
        ring_frames,
        buffer_frames: None,
    }
}

#[test]
fn manual_clock_counts_rendered_frames() {
    let (mut output, _feeder, clock) = OfflineOutput::new(&config(4096));
    assert_eq!(clock.sample_rate(), 48_000);
    assert_eq!(clock.now(), 0.0);
    output.advance(480);
    assert_eq!(clock.frames_written(), 480);
    assert_eq!(clock.now(), 0.01);
    output.advance(48_000 - 480);
    assert_eq!(clock.now(), 1.0);
    let mut block = vec![0.0f32; 2 * 24_000];
    output.render(&mut block);
    assert_eq!(clock.frames_written(), 72_000);
    assert_eq!(clock.now(), 1.5);
    // A manual clock has no latency and ignores the instant it is asked about.
    assert_eq!(clock.latency().as_nanos(), 0);
    assert_eq!(clock.at(std::time::Instant::now()), 1.5);
    assert_eq!(clock.rate_ratio(), 1.0);
}

#[test]
fn planar_audio_comes_out_interleaved_in_order() {
    let (mut output, mut feeder, clock) = OfflineOutput::new(&config(1000));
    let left: Vec<f32> = (0..700).map(|i| i as f32).collect();
    let right: Vec<f32> = (0..700).map(|i| -(i as f32)).collect();
    assert_eq!(feeder.push_planar(&[&left, &right]), 700);
    assert_eq!(feeder.queued_frames(), 700);
    let mut out = vec![9.0f32; 2 * 500];
    output.render(&mut out);
    for i in 0..500 {
        assert_eq!((out[2 * i], out[2 * i + 1]), (i as f32, -(i as f32)));
    }
    // The ring wraps: 800 more frames go in around its end.
    let left: Vec<f32> = (700..1500).map(|i| i as f32).collect();
    let right: Vec<f32> = (700..1500).map(|i| -(i as f32)).collect();
    assert_eq!(feeder.push_planar(&[&left, &right]), 800);
    let mut out = vec![9.0f32; 2 * 1000];
    output.render(&mut out);
    for (i, frame) in out.chunks_exact(2).enumerate() {
        let n = (500 + i) as f32;
        assert_eq!(frame, [n, -n]);
    }
    assert_eq!(clock.underruns(), 0);
}

#[test]
fn a_full_ring_takes_what_fits_and_never_blocks() {
    let (_output, mut feeder, _clock) = OfflineOutput::new(&config(256));
    let samples = vec![0.5f32; 2 * 1000];
    assert_eq!(feeder.push_interleaved(&samples), 256);
    assert_eq!(feeder.free_frames(), 0);
    assert_eq!(feeder.push_interleaved(&samples), 0);
    assert_eq!(feeder.push_planar(&[&samples, &samples]), 0);
}

#[test]
fn underruns_play_silence_and_are_counted() {
    let (mut output, mut feeder, clock) = OfflineOutput::new(&config(4096));
    feeder.push_interleaved(&vec![0.25f32; 2 * 300]);
    let mut out = vec![9.0f32; 2 * 512];
    output.render(&mut out);
    assert!(out[..600].iter().all(|&s| s == 0.25));
    assert!(out[600..].iter().all(|&s| s == 0.0));
    assert_eq!(clock.underruns(), 1);
    assert_eq!(clock.silent_frames(), 212);
    // The clock keeps counting through silence.
    assert_eq!(clock.frames_written(), 512);

    // An empty ring while feeding is another underrun.
    output.render(&mut out);
    assert_eq!(clock.underruns(), 2);
    assert_eq!(clock.silent_frames(), 212 + 512);

    // Paused: silence, but no underruns.
    feeder.set_feeding(false);
    output.advance(48_000);
    assert_eq!(clock.underruns(), 2);
    assert_eq!(clock.frames_written(), 1024 + 48_000);
}

#[test]
fn feeder_channels_map_onto_the_output() {
    // Missing planes queue silence; extra planes are ignored.
    let (mut output, mut feeder, _clock) = OfflineOutput::new(&config(64));
    let a = [1.0f32; 16];
    assert_eq!(feeder.push_planar(&[&a]), 16);
    let mut out = vec![9.0f32; 32];
    output.render(&mut out);
    assert!(out.chunks_exact(2).all(|f| f == [1.0, 0.0]));
    let b = [2.0f32; 16];
    let c = [3.0f32; 16];
    assert_eq!(feeder.push_planar(&[&a, &b, &c]), 16);
    output.render(&mut out);
    assert!(out.chunks_exact(2).all(|f| f == [1.0, 2.0]));
}

#[test]
fn system_clock_stands_in_without_a_device() {
    let clock = audio::Clock::system(48_000);
    let start = std::time::Instant::now();
    let later = start + std::time::Duration::from_millis(250);
    let at = clock.at(later);
    assert!((0.25..0.26).contains(&at), "{at}");
    assert!(clock.now() <= clock.at(later));
    assert_eq!((clock.frames_written(), clock.underruns()), (0, 0));
    assert_eq!(clock.rate_ratio(), 1.0);
}
