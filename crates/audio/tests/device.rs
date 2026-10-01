//! Opt-in check against the real default output device: plays about 1.5 s of silence and
//! checks the clock advances at the device rate. Runs only with
//! `GLASS_AUDIO_DEVICE_TEST=1` (CI machines have no audio device and a test should not
//! make sound by surprise); skips when no device opens.

use std::time::{Duration, Instant};

use audio::{OutputConfig, OutputError, OutputStream};

#[test]
fn clock_advances_at_the_device_rate() {
    if std::env::var("GLASS_AUDIO_DEVICE_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set GLASS_AUDIO_DEVICE_TEST=1 to play silence on the default device");
        return;
    }
    let (stream, feeder, clock) = match OutputStream::open_default(&OutputConfig::default()) {
        Ok(opened) => opened,
        Err(OutputError::NoDevice) => {
            eprintln!("skipped: no output device");
            return;
        }
        Err(error) => panic!("opening the default output failed: {error}"),
    };
    // Silence: nothing is fed, and a dry ring is not an underrun while not feeding.
    feeder.set_feeding(false);
    let rate = f64::from(clock.sample_rate());

    std::thread::sleep(Duration::from_millis(300));
    let (t0, frames0, secs0) = (Instant::now(), clock.frames_written(), clock.now());
    let mut last = secs0;
    for _ in 0..120 {
        std::thread::sleep(Duration::from_millis(10));
        let secs = clock.now();
        assert!(secs >= last, "clock went back: {last} then {secs}");
        last = secs;
    }
    let (t1, frames1, secs1) = (Instant::now(), clock.frames_written(), clock.now());
    let elapsed = t1.duration_since(t0).as_secs_f64();
    let frame_rate = (frames1 - frames0) as f64 / elapsed;
    let clock_rate = (secs1 - secs0) / elapsed;
    eprintln!(
        "device: {} Hz, {} channels, latency {:.2} ms, frames at {frame_rate:.0}/s, clock at {clock_rate:.5} s/s, rate ratio {:.6}, errors {}, xruns {}",
        clock.sample_rate(),
        stream.device_channels(),
        clock.latency().as_secs_f64() * 1000.0,
        clock.rate_ratio(),
        stream.errors(),
        stream.xruns()
    );
    // Callbacks deliver whole buffers (about 10 ms), so the frame count is coarse over 1.2 s.
    assert!(
        (frame_rate / rate - 1.0).abs() < 0.03,
        "frames at {frame_rate}/s, device rate {rate}"
    );
    // The drift-corrected clock is smooth: within 1% of real time over the window.
    assert!(
        (clock_rate - 1.0).abs() < 0.01,
        "clock at {clock_rate} s per s"
    );
    assert_eq!(clock.underruns(), 0);
    // Overloads (xruns) depend on how busy the machine is, so only real errors fail.
    assert_eq!(stream.errors(), 0);
}
