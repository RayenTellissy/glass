//! Embedded subtitles: cues pushed into an export session come back out of the file.
//! Each container is written with a short H.264 picture stream and one or two text
//! subtitle streams, then re-probed (codec, language, title) and its subtitle streams
//! decoded with FFmpeg: every cue's start, end and text must match what was pushed.

use std::path::{Path, PathBuf};

use media::export::{
    Container, EncoderPreference, ExportError, ExportSession, ExportSettings, Picture,
    SubtitleSettings, VideoCodec, VideoSettings,
};
use media::{MediaKind, Rational, SubtitleCue, probe, read_subtitle_cues};

const W: u32 = 160;
const H: u32 = 90;
const FPS: Rational = Rational::new(10, 1);
const FRAMES: u64 = 40;

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("glass-subtitle-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

fn cue(start_ms: i64, end_ms: i64, text: &str) -> SubtitleCue {
    SubtitleCue {
        start_ms,
        end_ms,
        text: text.to_string(),
    }
}

/// Cues with a gap, back-to-back cues, two lines, characters that are markup in ASS and
/// WebVTT, non-Latin text, and a cue ending with the video.
fn french() -> Vec<SubtitleCue> {
    vec![
        cue(0, 1000, "Bonjour"),
        cue(1500, 2033, "Deux\nlignes"),
        cue(2033, 2900, "Fish & chips <b>pas gras</b> -->"),
        cue(2900, 3400, "{\\i1} pas une balise, back\\slash \\N"),
        cue(3400, 4000, "日本語のテキスト"),
    ]
}

fn english() -> Vec<SubtitleCue> {
    vec![cue(250, 750, "Hello"), cue(3000, 3999, "Goodbye")]
}

fn settings(container: Container, preference: EncoderPreference) -> ExportSettings {
    let mut video = VideoSettings::new(VideoCodec::H264, W, H, FPS);
    video.encoder_preference = preference;
    ExportSettings::new(container)
        .with_video(video)
        .with_subtitles(SubtitleSettings::new("fra").with_title("Français"))
        .with_subtitles(SubtitleSettings::new("eng"))
}

/// Writes the pictures with the cues pushed first (`cues_first`) or between frames at
/// their start times.
fn export(path: &Path, settings: &ExportSettings, cues_first: bool) {
    let streams = [french(), english()];
    let mut session = ExportSession::create(path, settings).expect("export opens");
    let mut next = [0usize; 2];
    let mut push_until = |session: &mut ExportSession, ms: i64| {
        for (stream, cues) in streams.iter().enumerate() {
            while let Some(c) = cues.get(next[stream]).filter(|c| c.start_ms <= ms) {
                session.push_subtitle(stream, c).expect("cue");
                next[stream] += 1;
            }
        }
    };
    if cues_first {
        push_until(&mut session, i64::MAX);
    }
    let grey = vec![128u8; (W * H * 4) as usize];
    for n in 0..FRAMES {
        push_until(&mut session, n as i64 * 100);
        let picture = Picture::rgba8(W, H, &grey, W as usize * 4);
        session.push_picture(&picture).expect("picture");
    }
    push_until(&mut session, i64::MAX);
    let summary = session.finish().expect("export finishes");
    assert_eq!(summary.subtitle_cues, 7);
    assert_eq!(summary.frames, FRAMES);
}

fn check(path: &Path, codec: &str) {
    let info = probe(path).expect("probe");
    let subtitles: Vec<_> = info
        .streams
        .iter()
        .filter(|s| s.kind == MediaKind::Subtitle)
        .collect();
    assert_eq!(subtitles.len(), 2, "{:#?}", info.streams);
    for s in &subtitles {
        assert_eq!(s.codec.name, codec);
    }
    assert_eq!(subtitles[0].language.as_deref(), Some("fra"));
    assert_eq!(subtitles[1].language.as_deref(), Some("eng"));
    let titled = subtitles[0]
        .tag("title")
        .or(subtitles[0].tag("handler_name"));
    assert_eq!(titled, Some("Français"), "{:?}", subtitles[0].tags);

    let read = read_subtitle_cues(path, subtitles[0].index).expect("read cues");
    assert_eq!(read, french());
    let read = read_subtitle_cues(path, subtitles[1].index).expect("read cues");
    assert_eq!(read, english());
}

#[test]
fn mov_text_in_mp4_round_trips() {
    let dir = scratch("mp4");
    // Auto tries VideoToolbox on macOS first: the cues wait for the encoder's trial.
    let path = dir.join("subtitles.mp4");
    export(
        &path,
        &settings(Container::Mp4, EncoderPreference::Auto),
        true,
    );
    check(&path, "mov_text");
    let path = dir.join("interleaved.mp4");
    export(
        &path,
        &settings(Container::Mp4, EncoderPreference::Software),
        false,
    );
    check(&path, "mov_text");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mov_text_in_mov_round_trips() {
    let dir = scratch("mov");
    let path = dir.join("subtitles.mov");
    export(
        &path,
        &settings(Container::Mov, EncoderPreference::Software),
        false,
    );
    check(&path, "mov_text");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn webvtt_in_matroska_round_trips() {
    let dir = scratch("mkv");
    let path = dir.join("subtitles.mkv");
    export(
        &path,
        &settings(Container::Matroska, EncoderPreference::Software),
        true,
    );
    check(&path, "webvtt");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn subtitle_settings_and_cues_are_checked() {
    assert!(Container::WebM.accepts_subtitles());
    assert!(!Container::Gif.accepts_subtitles());
    let video = VideoSettings::new(VideoCodec::Gif, W, H, FPS);
    let gif = ExportSettings::new(Container::Gif)
        .with_video(video)
        .with_subtitles(SubtitleSettings::new("eng"));
    assert!(matches!(
        gif.validate(),
        Err(ExportError::Incompatible {
            codec: "text subtitles",
            ..
        })
    ));
    for language in ["en", "EN G", "Eng", "english"] {
        let mut s = settings(Container::Mp4, EncoderPreference::Software);
        s.subtitles[0].language = language.to_string();
        assert!(
            matches!(s.validate(), Err(ExportError::Settings(_))),
            "{language}"
        );
    }

    let dir = scratch("checks");
    let path = dir.join("checks.mkv");
    let settings = settings(Container::Matroska, EncoderPreference::Software);
    let mut session = ExportSession::create(&path, &settings).expect("export opens");
    session
        .push_subtitle(0, &cue(1000, 2000, "first"))
        .expect("cue");
    for bad in [
        cue(1500, 2500, "overlaps"),
        cue(500, 900, "earlier"),
        cue(3000, 3000, "empty"),
    ] {
        assert!(
            matches!(session.push_subtitle(0, &bad), Err(ExportError::Input(_))),
            "{bad:?}"
        );
    }
    assert!(matches!(
        session.push_subtitle(2, &cue(0, 1, "no stream")),
        Err(ExportError::Input(_))
    ));
    // The other stream is independent.
    session.push_subtitle(1, &cue(0, 500, "ok")).expect("cue");
    drop(session);
    let _ = std::fs::remove_dir_all(dir);
}
