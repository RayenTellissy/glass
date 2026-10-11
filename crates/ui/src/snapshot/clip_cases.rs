//! Snapshot cases for the clips in the track lanes: video clips with thumbnails at a
//! close and a medium zoom (a 200% clip with effects, a disabled one, a keyframed one,
//! a selected one), audio clips with mono and stereo waveforms at three gains, nested
//! and compound clips with transitions at each alignment, the whole of a long edit as
//! plain blocks, a right-to-left locale, and clips whose analysis is still loading.
//!
//! Thumbnails and peaks are made up here: atlases of tiles painted from the theme's
//! colours, and peaks of a deterministic signal, inserted into [`ClipMedia`] as the
//! analysis feed would.

use std::sync::Arc;

use egui::vec2;
use engine::media::RgbaImage;
use engine::media::analysis::peaks::{LEVELS, Peak, PeakLevel, Peaks};
use engine::media::analysis::thumbnails::{
    ATLAS_COLUMNS, AtlasFormat, PER_ATLAS, THUMBNAILS_VERSION, ThumbnailLevel, ThumbnailStrips,
};
use project::params::{ParamValue, builtin};
use project::{
    Alignment, ClipParam, ClipSpec, ColourLabel, Item, ItemId, KeyTarget, MediaId, Project,
    Rational, Sequence, SequenceFormat, SequenceId, SpeedKeep, Time, TimeRange, TrackId, TrackKind,
    TransitionSpec,
};

use crate::panels::timeline::clip_media::{AtlasKey, ClipMedia};
use crate::panels::timeline::clips::{self, ClipHooks, ClipsView};
use crate::panels::timeline::view::{self, Extent, TimelineView, TrackLayout, ViewPos};
use crate::t;
use crate::theme::{self, Role};

pub(super) const CLIPS_SIZE: egui::Vec2 = vec2(640.0, 124.0);
pub(super) const BLOCKS_SIZE: egui::Vec2 = vec2(640.0, 112.0);

const RATE: Rational = Rational::new(25, 1);
/// Thumbnail size in the made-up strips (16:9).
const THUMB: (u32, u32) = (64, 36);

fn frames(n: i64) -> Time {
    Time::from_frames(n, RATE).expect("time")
}

fn range(a: i64, b: i64) -> TimeRange {
    TimeRange::between(frames(a), frames(b)).expect("range")
}

/// A project being built: its one sequence and the media it references.
struct Sample {
    p: Project,
    seq: SequenceId,
    rows: Vec<(TrackId, f32)>,
}

impl Sample {
    fn new() -> Self {
        let p = Project::new("uid-clips", "Clips");
        let format = SequenceFormat {
            frame_rate: RATE,
            ..SequenceFormat::default()
        };
        let (p, seq) = p.add_sequence_with("Main", format).expect("sequence");
        Self {
            p,
            seq,
            rows: Vec::new(),
        }
    }

    fn media(&mut self, name: &str) -> MediaId {
        let (p, id) = self
            .p
            .add_media(format!("hash-{name}"), format!("/Volumes/Card A/{name}"))
            .expect("media");
        self.p = p;
        id
    }

    fn track(&mut self, kind: TrackKind, height: f32) -> TrackId {
        let name = format!("{kind:?}{}", self.rows.len());
        let (p, id) = self
            .p
            .add_track(self.seq, kind, &name, None)
            .expect("track");
        self.p = p;
        self.rows.push((id, height));
        id
    }

    /// A clip from 10 s into `media` (room for transition handles).
    fn clip(&mut self, track: TrackId, media: MediaId, a: i64, b: i64) -> ItemId {
        let spec = ClipSpec {
            media,
            range: range(a, b),
            source_start: Time::seconds(10),
        };
        let (p, id) = self.p.insert_clip(track, spec).expect("clip");
        self.p = p;
        id
    }

    fn edit(&mut self, f: impl FnOnce(&Project) -> Project) {
        self.p = f(&self.p);
    }

    fn sequence(&self) -> Arc<Sequence> {
        self.p.sequence(self.seq).expect("sequence").clone()
    }
}

/// A made-up strip for a clip of `seconds`: every level, all atlases named.
fn strips(seconds: u32) -> ThumbnailStrips {
    let levels = [1u32, 5, 30]
        .iter()
        .enumerate()
        .map(|(level, &interval)| {
            let n = seconds.div_ceil(interval).max(1);
            ThumbnailLevel {
                level: level as u32,
                interval,
                times_us: (0..n)
                    .map(|i| i64::from(i * interval) * 1_000_000)
                    .collect(),
                atlases: (0..n.div_ceil(PER_ATLAS))
                    .map(|c| format!("l{level}.c{c}.jpg"))
                    .collect(),
            }
        })
        .collect();
    ThumbnailStrips {
        version: THUMBNAILS_VERSION,
        width: THUMB.0,
        height: THUMB.1,
        format: AtlasFormat::Jpeg,
        columns: ATLAS_COLUMNS,
        per_atlas: PER_ATLAS,
        levels,
    }
}

/// One atlas of made-up footage: each tile a sky over a ground with a subject that
/// moves across with the tile's time, in the theme's colours.
fn atlas(ctx: &egui::Context, seed: u32, level: u32, chunk: u32) -> RgbaImage {
    let theme = theme::current(ctx);
    let palette = [
        Role::ScopeBlue,
        Role::ClipTitle,
        Role::ScopeGreen,
        Role::ClipAdjustment,
    ];
    let sky = theme.color(palette[seed as usize % palette.len()]);
    let ground = theme.color(Role::Bg2);
    let subject = theme.color(palette[(seed as usize + 1) % palette.len()]);
    let (w, h) = (THUMB.0 * ATLAS_COLUMNS, THUMB.1 * PER_ATLAS / ATLAS_COLUMNS);
    let mut data = vec![0u8; (w * h * 4) as usize];
    let interval = [1u32, 5, 30][level as usize];
    for py in 0..h {
        for px in 0..w {
            let (col, row) = (px / THUMB.0, py / THUMB.1);
            let index = chunk * PER_ATLAS + row * ATLAS_COLUMNS + col;
            let seconds = (index * interval) as f32;
            let (x, y) = (px % THUMB.0, py % THUMB.1);
            let fy = y as f32 / THUMB.1 as f32;
            let horizon = 0.55 + 0.1 * ((seconds * 0.15 + seed as f32).sin());
            let centre = (seconds * 0.07 + seed as f32 * 0.3).fract() * THUMB.0 as f32;
            let c = if (x as f32 - centre).abs() < 5.0 && fy > horizon - 0.25 {
                subject
            } else if fy < horizon {
                sky.gamma_multiply(1.0 - 0.5 * fy)
            } else {
                ground
            };
            let i = ((py * w + px) * 4) as usize;
            data[i..i + 4].copy_from_slice(&[c.r(), c.g(), c.b(), 255]);
        }
    }
    RgbaImage {
        width: w,
        height: h,
        data,
    }
}

/// Made-up peaks at 48 kHz: phrases of a few seconds with pauses, louder in `seed`'s
/// rhythm; the right channel a little quieter.
fn peaks(seconds: u32, channels: usize, seed: u32) -> Peaks {
    let rate = 48_000u32;
    let total = u64::from(seconds) * u64::from(rate);
    let n0 = total.div_ceil(u64::from(LEVELS[0])) as usize;
    let mut level0 = Vec::with_capacity(n0 * channels);
    for i in 0..n0 {
        let t = i as f32 * LEVELS[0] as f32 / rate as f32;
        let phrase = ((t * 0.9 + seed as f32).sin() * 0.5 + 0.5).powf(1.5);
        let pause = if (t * 0.35 + seed as f32 * 0.7).sin() > 0.75 {
            0.05
        } else {
            1.0
        };
        let syllable = (t * 7.0 + seed as f32).sin().abs() * 0.6 + 0.4;
        for ch in 0..channels {
            let a = (phrase * pause * syllable * if ch == 1 { 0.8 } else { 0.95 }).min(1.0);
            let q = |v: f32| (v * 32767.0) as i16;
            level0.push(Peak {
                min: q(-a),
                max: q(a * 0.92),
                rms: q(a * 0.45),
            });
        }
    }
    let mut levels = vec![PeakLevel {
        samples_per_peak: LEVELS[0],
        peaks: level0,
    }];
    for &spp in &LEVELS[1..] {
        let prev = &levels.last().expect("level").peaks;
        let count = prev.len() / channels;
        let mut next = Vec::with_capacity(count.div_ceil(4) * channels);
        for j in 0..count.div_ceil(4) {
            for ch in 0..channels {
                let group = (j * 4..(j * 4 + 4).min(count)).map(|k| prev[k * channels + ch]);
                let mut p = Peak::default();
                for g in group {
                    p.min = p.min.min(g.min);
                    p.max = p.max.max(g.max);
                    p.rms = p.rms.max(g.rms);
                }
                next.push(p);
            }
        }
        levels.push(PeakLevel {
            samples_per_peak: spp,
            peaks: next,
        });
    }
    Peaks {
        sample_rate: rate,
        channels,
        total_samples: total,
        levels,
    }
}

/// Fills `media` with strips and every atlas for the video sources, peaks for the
/// audio ones.
fn load(ctx: &egui::Context, video: &[MediaId], audio: &[(MediaId, usize)]) -> ClipMedia {
    let mut media = ClipMedia::new();
    for (seed, &id) in video.iter().enumerate() {
        let s = Arc::new(strips(120));
        media.insert_strips(id, s.clone());
        for (level, l) in s.levels.iter().enumerate() {
            for chunk in 0..l.atlases.len() {
                let key = AtlasKey {
                    level: level as u32,
                    chunk: chunk as u32,
                };
                let image = atlas(ctx, seed as u32, level as u32, chunk as u32);
                media.insert_atlas(ctx, id, key, &image);
            }
        }
    }
    for (seed, &(id, channels)) in audio.iter().enumerate() {
        media.insert_peaks(id, Arc::new(peaks(120, channels, seed as u32 + 1)));
    }
    media
}

/// Selection and labels for the cases.
struct Hooks {
    selected: Vec<ItemId>,
    labels: Vec<(ItemId, ColourLabel)>,
}

impl ClipHooks for Hooks {
    fn is_selected(&self, item: ItemId) -> bool {
        self.selected.contains(&item)
    }

    fn label(&self, item: &Item) -> Option<ColourLabel> {
        self.labels
            .iter()
            .find(|(i, _)| *i == item.id)
            .map(|(_, l)| *l)
    }
}

const NO_HOOKS: Hooks = Hooks {
    selected: Vec::new(),
    labels: Vec::new(),
};

/// Draws the lanes and clips of `s` filling the case, the left edge at `start` frames
/// with `seconds` across.
fn draw(
    ui: &mut egui::Ui,
    s: &Sample,
    media: &mut ClipMedia,
    hooks: &Hooks,
    start: f64,
    seconds: f64,
    rtl: bool,
) {
    let seq = s.sequence();
    let layout = TrackLayout::new(s.rows.iter().copied());
    let bg = theme::current(ui.ctx()).color(Role::Bg1);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(bg))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let mut v = TimelineView::for_sequence(&seq);
            v.set_extent(Extent {
                width: rect.width(),
                height: rect.height(),
                duration: view::sequence_duration(&seq),
                content_height: layout.content_height(),
            });
            v.set_pos(ViewPos {
                start,
                frame_px: f64::from(rect.width()) / (seconds * 25.0),
            });
            view::lanes(ui, rect, &v, &layout);
            let cv = ClipsView {
                project: &s.p,
                sequence: &seq,
                rtl,
            };
            let _ = clips::clips(ui, rect, &v, &layout, &cv, media, hooks);
        });
}

/// Two video tracks: a plain clip, a selected 200% one with two effects, a disabled
/// one, a keyframed one with a colour label, and a 50% one cut short.
fn video_sample() -> (Sample, Vec<MediaId>, Hooks) {
    let mut s = Sample::new();
    let names = [
        t!("sample.clip.interview"),
        t!("sample.clip.broll"),
        t!("sample.clip.harbour"),
    ];
    let media: Vec<MediaId> = names.iter().map(|n| s.media(&format!("{n}.mov"))).collect();
    let v2 = s.track(TrackKind::Video, 48.0);
    let v1 = s.track(TrackKind::Video, 72.0);
    let a = s.clip(v1, media[0], 0, 150);
    let b = s.clip(v1, media[1], 150, 300);
    let c = s.clip(v1, media[2], 300, 450);
    let d = s.clip(v2, media[1], 50, 200);
    let e = s.clip(v2, media[2], 225, 400);
    s.edit(|p| {
        p.set_clip_speed(b, Rational::new(2, 1), SpeedKeep::Timeline)
            .expect("speed")
    });
    s.edit(|p| {
        let (p, _) = p.add_effect(b, &builtin::crop(), None).expect("fx");
        p.add_effect(b, &builtin::compositing(), None)
            .expect("fx")
            .0
    });
    s.edit(|p| p.set_clip_enabled(c, false).expect("disable"));
    s.edit(|p| {
        let target = KeyTarget::Clip {
            item: d,
            param: ClipParam::Opacity,
        };
        p.animate_param(target, Time::seconds(10))
            .and_then(|p| p.set_key(target, Time::seconds(12), ParamValue::Float(0.5)))
            .expect("key")
    });
    s.edit(|p| {
        p.set_clip_speed(e, Rational::new(1, 2), SpeedKeep::Timeline)
            .expect("speed")
    });
    let _ = a;
    let hooks = Hooks {
        selected: vec![b],
        labels: vec![(d, ColourLabel::Amber)],
    };
    (s, media, hooks)
}

/// The video clips with 18 seconds across.
pub(super) fn video(ui: &mut egui::Ui) {
    let (s, ids, hooks) = video_sample();
    let mut media = load(ui.ctx(), &ids, &[]);
    draw(ui, &s, &mut media, &hooks, -10.0, 19.0, false);
}

/// The same clips close up: about four seconds across, from 4.6 s.
pub(super) fn video_close(ui: &mut egui::Ui) {
    let (s, ids, hooks) = video_sample();
    let mut media = load(ui.ctx(), &ids, &[]);
    draw(ui, &s, &mut media, &hooks, 115.0, 4.2, false);
}

/// The video clips in a right-to-left locale: names at the right, badges at the left.
pub(super) fn video_rtl(ui: &mut egui::Ui) {
    let (s, ids, hooks) = video_sample();
    let mut media = load(ui.ctx(), &ids, &[]);
    draw(ui, &s, &mut media, &hooks, -10.0, 19.0, true);
}

/// The video clips before any analysis arrived: placeholders under the names.
pub(super) fn loading(ui: &mut egui::Ui) {
    let (s, _, hooks) = video_sample();
    let mut media = ClipMedia::new();
    draw(ui, &s, &mut media, &hooks, -10.0, 19.0, false);
}

/// Dialogue in stereo at 0 dB and -12 dB, music in mono at +6 dB with a 150% clip, a
/// keyframed gain, and a disabled clip.
pub(super) fn audio(ui: &mut egui::Ui) {
    let mut s = Sample::new();
    let voice = s.media(&format!("{}.wav", t!("sample.clip.interview")));
    let memo = s.media(&format!("{}.m4a", t!("sample.clip.voice_memo")));
    let music = s.media(&format!("{}.wav", t!("sample.clip.music")));
    let a1 = s.track(TrackKind::Audio, 72.0);
    let a2 = s.track(TrackKind::Audio, 48.0);
    let _ = s.clip(a1, voice, 0, 200);
    let quiet = s.clip(a1, memo, 200, 330);
    let off = s.clip(a1, voice, 340, 460);
    let loud = s.clip(a2, music, 20, 250);
    let fast = s.clip(a2, music, 260, 400);
    s.edit(|p| p.set_clip_gain(quiet, -12.0).expect("gain"));
    s.edit(|p| p.set_clip_enabled(off, false).expect("disable"));
    s.edit(|p| p.set_clip_gain(loud, 6.0).expect("gain"));
    s.edit(|p| {
        let target = KeyTarget::Clip {
            item: fast,
            param: ClipParam::GainDb,
        };
        p.animate_param(target, Time::seconds(10))
            .and_then(|p| p.set_key(target, Time::new(29, 2)?, ParamValue::Float(-30.0)))
            .expect("key")
    });
    s.edit(|p| {
        p.set_clip_speed(fast, Rational::new(3, 2), SpeedKeep::Timeline)
            .expect("speed")
    });
    let mut media = load(ui.ctx(), &[], &[(voice, 2), (memo, 1), (music, 1)]);
    draw(ui, &s, &mut media, &NO_HOOKS, -10.0, 19.0, false);
}

/// A nested sequence and a compound clip, and transitions centred on, starting at and
/// ending at their cuts, plus a fade in from nothing.
pub(super) fn nested_and_transitions(ui: &mut egui::Ui) {
    let mut s = Sample::new();
    let names = [t!("sample.clip.interview"), t!("sample.clip.broll")];
    let media: Vec<MediaId> = names.iter().map(|n| s.media(&format!("{n}.mov"))).collect();
    let (p, inner) =
        s.p.add_sequence_with(
            t!("sample.recovery.sequence"),
            SequenceFormat {
                frame_rate: RATE,
                ..SequenceFormat::default()
            },
        )
        .expect("nested");
    s.p = p;
    let v2 = s.track(TrackKind::Video, 48.0);
    let v1 = s.track(TrackKind::Video, 72.0);
    let a = s.clip(v1, media[0], 0, 110);
    let b = s.clip(v1, media[1], 110, 230);
    let c = s.clip(v1, media[0], 230, 340);
    let d = s.clip(v1, media[1], 340, 460);
    let (p, nested) =
        s.p.insert_sequence_clip(v2, inner, range(40, 200), Time::ZERO)
            .expect("nested clip");
    s.p = p;
    let e = s.clip(v2, media[1], 260, 420);
    let _ = nested;
    for (out, inc, align, len) in [
        (Some(a), Some(b), Alignment::Centre, 24),
        (Some(b), Some(c), Alignment::Start, 20),
        (Some(c), Some(d), Alignment::End, 20),
        (None, Some(e), Alignment::Start, 25),
    ] {
        s.edit(|p| {
            let spec = TransitionSpec {
                alignment: align,
                ..TransitionSpec::new(out, inc, frames(len))
            };
            p.add_transition(spec).expect("transition").0
        });
    }
    let mut media_store = load(ui.ctx(), &media, &[]);
    draw(ui, &s, &mut media_store, &NO_HOOKS, -10.0, 19.0, false);
}

/// Two hours of 200 clips over four tracks with the whole edit on screen: plain blocks.
pub(super) fn blocks(ui: &mut egui::Ui) {
    let mut s = Sample::new();
    let m = s.media("A001.mov");
    let step = 2 * 60 * 60 * 25 / 50;
    for (kind, offset) in [
        (TrackKind::Video, 7),
        (TrackKind::Video, 0),
        (TrackKind::Audio, 0),
        (TrackKind::Audio, 13),
    ] {
        let track = s.track(kind, 24.0);
        for i in 0..50 {
            let a = i * step + offset * (i % 3) * 25;
            let b = a + step - 25 * (1 + i % 4) - offset * (i % 3) * 25;
            let id = s.clip(track, m, a, b);
            if i % 9 == 4 {
                s.edit(|p| p.set_clip_enabled(id, false).expect("disable"));
            }
        }
    }
    let mut media = ClipMedia::new();
    draw(
        ui,
        &s,
        &mut media,
        &NO_HOOKS,
        -900.0,
        2.0 * 3600.0 + 80.0,
        false,
    );
}
