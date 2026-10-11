//! The export queue's audio is the sequence mix: an audio-only 32-bit float WAV export of
//! a range of tone clips reads back as `mix_range` of the same range, sample for sample.

mod common;
mod mix_common;

use std::time::{Duration, Instant};

use engine::audio::mixer::Layout;
use engine::export::{EventKind, ExportJob, ExportQueue, ExportQueueConfig};
use engine::media::export::{AudioCodec, AudioSettings, Container, ExportSettings};
use engine::media::{AudioOptions, AudioReader, ChannelLayout, PcmFormat};
use engine::mix::{MixFormat, MixSettings, mix_range_with};
use engine::project::TimeRange;
use mix_common::{Place, Tone, build, samples, temp_dir};

#[test]
fn the_export_carries_the_sequence_mix() {
    let Some(gpu) = common::gpu() else {
        eprintln!("skipped: no GPU adapter with 16-bit normalised textures");
        return;
    };
    let dir = temp_dir("export");
    let tone = Tone {
        rate: 48_000,
        seconds: 3.0,
        channels: vec![(440.0, 0.3), (330.0, 0.2)],
    };
    let path = dir.join("tone.wav");
    tone.write(&path);
    let built = build(
        std::slice::from_ref(&path),
        &[
            vec![Place {
                media: 0,
                start: 0.5,
                length: 2.0,
                source_start: 0.0,
            }],
            vec![Place {
                media: 0,
                start: 1.2,
                length: 1.0,
                source_start: 1.5,
            }],
        ],
    );
    let project = built
        .project
        .set_clip_gain(built.clips[1][0], -9.0)
        .unwrap();

    // Timeline frames 24..60 at 30 fps: 0.8 s to 2 s, across both clips.
    let output = dir.join("mix.wav");
    let settings = ExportSettings::new(Container::Wav).with_audio(AudioSettings::new(
        AudioCodec::Pcm(PcmFormat::F32),
        48_000,
        2,
    ));
    let job = ExportJob::new(project.clone(), built.sequence, settings, &output).with_range(24..60);
    let (queue, rx) = ExportQueue::new(&gpu.device, &gpu.queue, ExportQueueConfig::default());
    let id = queue.submit(job);
    let started = Instant::now();
    let finished = loop {
        let left = Duration::from_secs(60).saturating_sub(started.elapsed());
        let event = rx.recv_timeout(left).expect("an export event");
        if event.job != id {
            continue;
        }
        match event.kind {
            EventKind::Finished(f) => break f,
            EventKind::Failed { failure, .. } => panic!("the export failed: {failure:?}"),
            EventKind::Cancelled => panic!("the export was cancelled"),
            _ => {}
        }
    };
    assert!(!finished.silent_audio);

    let mut reader = AudioReader::open(
        &output,
        AudioOptions {
            sample_rate: 48_000,
            layout: ChannelLayout::Stereo,
            ..AudioOptions::default()
        },
    )
    .unwrap();
    let frames = 57_600;
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let read = reader.read(&mut [&mut left[..], &mut right[..]]).unwrap();
    assert_eq!(read, frames);

    let expected = mix_range_with(
        &project,
        built.sequence,
        TimeRange::new(samples(0.8), samples(1.2)),
        &MixSettings::default(),
        MixFormat {
            sample_rate: 48_000,
            layout: Layout::Stereo,
        },
    )
    .unwrap();
    assert_eq!(expected.len(), frames * 2);
    let worst = (0..frames)
        .map(|k| {
            (left[k] - expected[k * 2])
                .abs()
                .max((right[k] - expected[k * 2 + 1]).abs())
        })
        .fold(0.0f32, f32::max);
    assert!(worst <= 1e-6, "worst error {worst:e}");
    let peak = expected.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.3, "the range holds both clips: peak {peak}");
    let _ = std::fs::remove_dir_all(&dir);
}
