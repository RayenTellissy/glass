//! The canonical project file: pretty JSON (2-space indent, one key per line, trailing
//! newline), struct fields in declaration order, every map sorted by key, floats in their
//! shortest round-trip form. Saving is deterministic, and loading a file in any key order
//! or layout then saving gives the canonical bytes.
//!
//! `GLASS_UPDATE_FIXTURES=1` rewrites `tests/fixtures/canonical/key-order.proj`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use std::{env, fs};

use project::params::{ParamDef, ParamSchema, ParamValue, Range, effects};
use project::schema;
use project::{
    Alignment, BlendMode, ClipParam, ClipSource, ClipSpec, ColourLabel, Crop, EffectId, Handle,
    HandleSide, Interpolation, ItemId, ItemKind, KeyTarget, MarkerOwner, MarkerSpec, MediaId,
    Project, Rational, SequenceId, Time, TimeRange, TrackId, TrackKind, Transform, TransitionSpec,
};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);

/// Set in the child process of the cross-process determinism test: where it writes its
/// saved bytes.
const CHILD_OUT: &str = "GLASS_CANONICAL_CHILD_OUT";

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/canonical/key-order.proj")
}

/// A small deterministic generator, so a failure replays.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }

    /// A float in [0, 1) with a full 53-bit mantissa, so its shortest text form is long.
    fn unit(&mut self) -> f64 {
        let hi = self.next(1 << 27);
        let lo = self.next(1 << 26);
        ((hi << 26) | lo) as f64 / (1u64 << 53) as f64
    }

    /// 64 random bits (splitmix64 of the state).
    fn bits(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn pick<T: Copy>(&mut self, list: &[T]) -> T {
        list[self.next(list.len() as u64) as usize]
    }
}

fn frames(n: i64, rate: Rational) -> Time {
    Time::from_frames(n, rate).expect("frames")
}

fn later(t: Time, seconds: i64) -> Time {
    t.checked_add(Time::seconds(seconds)).expect("time")
}

/// An effect whose parameters are declared out of alphabetical order, so the file's
/// sorted `params` and `keyframes` maps differ from the declaration.
fn look() -> ParamSchema {
    ParamSchema::new(
        "acme.look",
        "acme.look",
        vec![
            ParamDef::float("zeta", "acme.look.zeta", 0.5, Range::new(0.0, 1.0)),
            ParamDef::point("mid", "acme.look.mid", [0.0, 0.0]),
            ParamDef::float("alpha", "acme.look.alpha", 1.0, Range::new(0.0, 4.0)),
            ParamDef::angle("hue", "acme.look.hue", 0.0),
        ],
    )
}

/// Animates `param` of `item` from its value at `at` to `value` one second later.
fn key_clip(p: Project, item: ItemId, param: ClipParam, at: Time, value: ParamValue) -> Project {
    let target = KeyTarget::Clip { item, param };
    p.animate_param(target, at)
        .and_then(|p| p.set_key(target, later(at, 1), value))
        .expect("clip keys")
}

fn key_effect(
    p: Project,
    effect: EffectId,
    schema: &ParamSchema,
    param: &str,
    at: Time,
    value: ParamValue,
) -> Project {
    let target = KeyTarget::Effect {
        effect,
        schema,
        param,
    };
    p.animate_param(target, at)
        .and_then(|p| p.set_key(target, later(at, 2), value))
        .expect("effect keys")
}

/// The project behind the committed key order golden: a few nodes of every kind with
/// maps whose keys are declared out of order, and floats with long shortest forms.
fn golden_project() -> Project {
    let p = Project::new("fixture-key-order", "Key order");
    let (p, a) = p.add_media("c0ffee", "/footage/pier.mov").expect("media");
    let (p, seq) = p.add_sequence("Pier", FPS, 48_000).expect("sequence");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let spec = |start, len, source| ClipSpec {
        media: a,
        range: TimeRange::new(frames(start, FPS), frames(len, FPS)),
        source_start: Time::seconds(source),
    };
    let (p, wide) = p.insert_clip(v1, spec(0, 50, 4)).expect("wide");
    let (p, close) = p.insert_clip(v1, spec(50, 50, 9)).expect("close");
    let (p, sound) = p.insert_clip(a1, spec(0, 100, 4)).expect("sound");
    let p = p.set_clip_opacity(wide, 0.1).expect("opacity");
    let p = p.set_clip_gain(sound, -1.0 / 3.0).expect("gain");
    let p = p
        .set_clip_transform(
            close,
            Transform {
                position: [1e-7, -123_456.789_012_345_67],
                scale: [2.0 / 3.0, 1.0],
                rotation: 359.999_999_999_999_94,
                anchor: [0.0, 0.5],
            },
        )
        .expect("transform");
    let p = p
        .set_clip_crop(
            close,
            Crop {
                left: 0.3,
                top: 0.0,
                right: 1.5,
                bottom: 0.0,
                feather: 2.25,
            },
        )
        .expect("crop");
    let p = p.set_clip_blend(close, BlendMode::Multiply).expect("blend");
    // Declared order: position, scale, rotation, crop.left, opacity; the file sorts them.
    let at = Time::seconds(4);
    let p = key_clip(
        p,
        wide,
        ClipParam::Position,
        at,
        ParamValue::Point([0.1, 0.2]),
    );
    let p = key_clip(p, wide, ClipParam::Scale, at, ParamValue::Point([1.5, 1.5]));
    let p = key_clip(p, wide, ClipParam::Rotation, at, ParamValue::Angle(-90.5));
    let p = key_clip(p, wide, ClipParam::CropLeft, at, ParamValue::Float(12.0));
    let p = key_clip(p, wide, ClipParam::Opacity, at, ParamValue::Float(0.7));
    let p = key_clip(p, sound, ClipParam::GainDb, at, ParamValue::Float(-6.02));
    let look = look();
    let (p, fx) = p.add_effect(wide, &look, None).expect("look");
    let p = p
        .set_effect_param(fx, &look, "mid", ParamValue::Point([0.25, -0.125]))
        .expect("mid");
    let p = key_effect(p, fx, &look, "zeta", at, ParamValue::Float(0.9));
    let p = key_effect(p, fx, &look, "alpha", at, ParamValue::Float(3.3));
    let p = p
        .set_key_interpolation(
            KeyTarget::Effect {
                effect: fx,
                schema: &look,
                param: "alpha",
            },
            at,
            Interpolation::ease(1),
        )
        .expect("ease");
    let lut = effects::lut();
    let (p, grade) = p.add_effect(close, &lut, None).expect("lut");
    let p = p
        .set_effect_param(grade, &lut, "intensity", ParamValue::Float(0.35))
        .expect("intensity");
    let (p, _) = p
        .add_transition(TransitionSpec {
            alignment: Alignment::Centre,
            easing: "ease-in-out".into(),
            ..TransitionSpec::new(Some(wide), Some(close), frames(10, FPS))
        })
        .expect("dissolve");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Sequence(seq),
            MarkerSpec {
                name: "Cue".into(),
                note: "über \"quoted\" \u{1F3AC}".into(),
                ..MarkerSpec::at(frames(25, FPS), ColourLabel::Amber)
            },
        )
        .expect("marker");
    let (p, _) = p
        .add_marker(
            MarkerOwner::Clip(close),
            MarkerSpec::at(Time::seconds(10), ColourLabel::Blue),
        )
        .expect("clip marker");
    p
}

/// Track ids of a sequence by kind, in order.
struct Seq {
    id: SequenceId,
    rate: Rational,
    video: Vec<TrackId>,
    audio: Vec<TrackId>,
}

#[derive(Default)]
struct Counts {
    clips: usize,
    transitions: usize,
    keyframed: usize,
    effects: usize,
    markers: usize,
}

/// Lays `n` clips end to end on `track` (with a gap now and then), with properties,
/// effects, keyframes, clip markers and transitions on some of the cuts.
#[allow(clippy::too_many_arguments)]
fn fill_track(
    mut p: Project,
    rng: &mut Lcg,
    media: &[MediaId],
    track: TrackId,
    rate: Rational,
    video: bool,
    n: usize,
    counts: &mut Counts,
) -> Project {
    let look = look();
    let lut = effects::lut();
    let mut cursor = 0i64;
    let mut previous: Option<(ItemId, i64)> = None;
    for i in 0..n {
        if rng.next(6) == 0 {
            cursor += 1 + rng.next(20) as i64;
        }
        let len = 24 + rng.next(30) as i64;
        let source = Time::seconds(2 + rng.next(60) as i64);
        let (next, item) = p
            .insert_clip(
                track,
                ClipSpec {
                    media: rng.pick(media),
                    range: TimeRange::new(frames(cursor, rate), frames(len, rate)),
                    source_start: source,
                },
            )
            .expect("clip");
        p = next;
        counts.clips += 1;
        if video {
            p = p.set_clip_opacity(item, rng.unit()).expect("opacity");
            p = p
                .set_clip_transform(
                    item,
                    Transform {
                        position: [(rng.unit() - 0.5) * 3840.0, (rng.unit() - 0.5) * 2160.0],
                        scale: [0.25 + 2.0 * rng.unit(), 0.25 + 2.0 * rng.unit()],
                        rotation: 720.0 * rng.unit() - 360.0,
                        anchor: [rng.unit(), -rng.unit()],
                    },
                )
                .expect("transform");
            if i % 4 == 0 {
                let (next, grade) = p.add_effect(item, &lut, None).expect("lut");
                p = next
                    .set_effect_param(grade, &lut, "intensity", ParamValue::Float(rng.unit()))
                    .expect("intensity");
                counts.effects += 1;
            }
            if i % 9 == 0 {
                let (next, fx) = p.add_effect(item, &look, None).expect("look");
                p = key_effect(
                    next,
                    fx,
                    &look,
                    "zeta",
                    source,
                    ParamValue::Float(rng.unit()),
                );
                p = key_effect(p, fx, &look, "hue", source, ParamValue::Angle(rng.unit()));
                counts.effects += 1;
            }
            if i % 5 == 0 {
                let opacity = ParamValue::Float(rng.unit());
                p = key_clip(p, item, ClipParam::Opacity, source, opacity);
                let at = ParamValue::Point([rng.unit() * 100.0, rng.unit() * -100.0]);
                p = key_clip(p, item, ClipParam::Position, source, at);
                p = p
                    .set_key_interpolation(
                        KeyTarget::Clip {
                            item,
                            param: ClipParam::Position,
                        },
                        source,
                        Interpolation::ease(2),
                    )
                    .expect("ease");
                let crop = ParamValue::Float(rng.unit() * 64.0);
                p = key_clip(p, item, ClipParam::CropLeft, source, crop);
                p = p
                    .set_key_handle(
                        KeyTarget::Clip {
                            item,
                            param: ClipParam::Position,
                        },
                        later(source, 1),
                        HandleSide::In,
                        Handle::new(vec![rng.unit(), -rng.unit()], rng.unit()),
                    )
                    .expect("handle");
                counts.keyframed += 1;
            }
        } else {
            p = p
                .set_clip_gain(item, -96.0 + 120.0 * rng.unit())
                .expect("gain");
            if i % 7 == 0 {
                let gain = ParamValue::Float(-24.0 * rng.unit());
                p = key_clip(p, item, ClipParam::GainDb, source, gain);
                counts.keyframed += 1;
            }
        }
        if i % 10 == 3 {
            let spec = MarkerSpec {
                name: format!("Note {i}"),
                ..MarkerSpec::at(source, ColourLabel::Green)
            };
            p = p
                .add_marker(MarkerOwner::Clip(item), spec)
                .expect("marker")
                .0;
            counts.markers += 1;
        }
        if let Some((out, end)) = previous
            && end == cursor
            && rng.next(3) == 0
        {
            let alignment = rng.pick(&[Alignment::Centre, Alignment::Start, Alignment::End]);
            p = p
                .add_transition(TransitionSpec {
                    alignment,
                    ..TransitionSpec::new(
                        Some(out),
                        Some(item),
                        frames(6 + 2 * rng.next(3) as i64, rate),
                    )
                })
                .expect("transition")
                .0;
            counts.transitions += 1;
        }
        previous = Some((item, cursor + len));
        cursor += len;
    }
    p
}

fn add_seq(p: Project, name: &str, rate: Rational, video: usize, audio: usize) -> (Project, Seq) {
    let (mut p, id) = p.add_sequence(name, rate, 48_000).expect("sequence");
    let mut seq = Seq {
        id,
        rate,
        video: Vec::new(),
        audio: Vec::new(),
    };
    for i in 0..video {
        let (next, t) = p
            .add_track(id, TrackKind::Video, format!("V{}", i + 1), None)
            .expect("video track");
        p = next;
        seq.video.push(t);
    }
    for i in 0..audio {
        let (next, t) = p
            .add_track(id, TrackKind::Audio, format!("A{}", i + 1), None)
            .expect("audio track");
        p = next;
        seq.audio.push(t);
    }
    (p, seq)
}

/// A project of 1000 media clips over three sequences (a 24 fps titles sequence nested
/// in the 25 fps main one, and an NTSC b-roll sequence), with transitions, keyframes,
/// effects, markers, nested sequence clips and a compound clip.
fn thousand_clips() -> (Project, Counts) {
    let mut rng = Lcg(0x1000_c11b);
    let mut counts = Counts::default();
    let mut p = Project::new("stress-1000", "Thousand clips");
    let mut media = Vec::new();
    for i in 0..24 {
        let (next, m) = p
            .add_media(
                format!("{:012x}", 0xabc000 + i),
                format!("/footage/roll {i:02}.mov"),
            )
            .expect("media");
        p = next;
        media.push(m);
    }
    let (p, titles) = add_seq(p, "Titles", Rational::new(24, 1), 1, 0);
    let (p, main) = add_seq(p, "Main", FPS, 3, 2);
    let (p, broll) = add_seq(p, "B-roll", Rational::new(30000, 1001), 1, 1);
    let mut p = fill_track(
        p,
        &mut rng,
        &media,
        titles.video[0],
        titles.rate,
        true,
        20,
        &mut counts,
    );
    for (track, video, n) in [
        (main.video[0], true, 300),
        (main.video[1], true, 200),
        (main.audio[0], false, 200),
        (main.audio[1], false, 50),
        (broll.video[0], true, 150),
        (broll.audio[0], false, 80),
    ] {
        // Audio items sit on the sample grid, which NTSC frames are not on.
        let rate = if broll.video.contains(&track) {
            broll.rate
        } else {
            main.rate
        };
        p = fill_track(p, &mut rng, &media, track, rate, video, n, &mut counts);
    }
    for i in 0..5 {
        let (next, _) = p
            .insert_sequence_clip(
                main.video[2],
                titles.id,
                TimeRange::new(frames(i * 100, FPS), frames(40, FPS)),
                Time::seconds(i),
            )
            .expect("titles clip");
        p = next;
    }
    // Nest three clips of V2 into a compound.
    let v2: Vec<ItemId> = p
        .find_track(main.video[1])
        .expect("v2")
        .1
        .items
        .iter()
        .skip(40)
        .take(3)
        .map(|i| i.id)
        .collect();
    let (mut p, compound, _) = p.nest_items(&v2, "Compound").expect("compound");
    for i in 0..30 {
        let spec = MarkerSpec {
            name: format!("Beat {i}"),
            chapter: i % 10 == 0,
            ..MarkerSpec::at(frames(i * 37, FPS), ColourLabel::Red)
        };
        p = p
            .add_marker(MarkerOwner::Sequence(main.id), spec)
            .expect("marker")
            .0;
        counts.markers += 1;
    }
    assert!(p.sequence(compound).expect("compound").compound);
    (p, counts)
}

/// Every media clip in the project, compound contents included.
fn media_clips(p: &Project) -> usize {
    p.sequences()
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|t| t.items.iter())
        .filter(
            |i| matches!(&i.kind, ItemKind::Clip(c) if matches!(c.source, ClipSource::Media(_))),
        )
        .count()
}

/// Checks the canonical layout line by line: 2-space indent steps, at most one key per
/// line, no trailing spaces, a trailing newline, and every map (`keyframes`, `params`)
/// with its keys in ascending byte order.
fn assert_canonical_layout(bytes: &[u8]) {
    let text = std::str::from_utf8(bytes).expect("utf-8");
    assert!(
        text.ends_with("}\n") && !text.ends_with("\n\n"),
        "one trailing newline"
    );
    let lines: Vec<&str> = text.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start_matches(' ').len();
    for (n, line) in lines.iter().enumerate() {
        assert!(
            !line.contains('\t') && !line.ends_with(' '),
            "line {n}: {line:?}"
        );
        assert_eq!(indent(line) % 2, 0, "line {n}: {line:?}");
        if n > 0 {
            assert!(
                indent(line) <= indent(lines[n - 1]) + 2,
                "line {n}: {line:?}"
            );
        }
        // A key line is `"key": value` with the value alone; a value line has no key.
        let body = line.trim_start();
        if body.starts_with('"') {
            let rest = &body[closing_quote(body) + 1..];
            assert!(
                rest.is_empty() || rest == "," || rest.starts_with(": "),
                "line {n}: {line:?}"
            );
            if let Some(value) = rest.strip_prefix(": ") {
                let value = value.trim_end_matches(',');
                let nested_key =
                    value.starts_with('"') && value[closing_quote(value) + 1..].starts_with(':');
                assert!(!nested_key, "two keys on line {n}: {line:?}");
            }
        }
    }
    for name in ["keyframes", "params"] {
        for keys in map_keys(text, name) {
            assert!(keys.is_sorted(), "unsorted {name} map: {keys:?}");
        }
    }
}

/// The keys of every non-empty object written as `"<name>": {`, in file order.
fn map_keys(text: &str, name: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start_matches(' ').len();
    let opening = format!("\"{name}\": {{");
    let mut maps = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if line.trim_start() != opening {
            continue;
        }
        let depth = indent(line) + 2;
        let keys: Vec<String> = lines[n + 1..]
            .iter()
            .take_while(|l| indent(l) >= depth)
            .filter(|l| indent(l) == depth && l.trim_start().starts_with('"'))
            .map(|l| {
                let b = l.trim_start();
                serde_json::from_str(&b[..=closing_quote(b)]).expect("key")
            })
            .collect();
        maps.push(keys);
    }
    maps
}

/// The index of the quote closing the string `s` starts with.
fn closing_quote(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 1;
    while bytes[i] != b'"' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    i
}

/// Writes `value` as compact JSON with every object's keys in reverse of their order in
/// `value` (a `serde_json::Map` keeps them sorted), the opposite of the canonical order
/// of maps and unrelated to the field order of structs.
fn reversed_compact(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().rev().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("key"));
                out.push(':');
                reversed_compact(v, out);
            }
            out.push('}');
        }
        Value::Array(list) => {
            out.push('[');
            for (i, v) in list.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                reversed_compact(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("scalar")),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[test]
fn a_thousand_clip_project_saves_loads_and_saves_to_the_same_bytes() {
    let (p, counts) = thousand_clips();
    assert!(media_clips(&p) >= 1000, "{} media clips", media_clips(&p));
    assert_eq!(counts.clips, 1000);
    assert!(
        counts.transitions >= 100,
        "{} transitions",
        counts.transitions
    );
    assert!(
        counts.keyframed >= 150,
        "{} keyframed clips",
        counts.keyframed
    );
    assert!(counts.effects >= 150, "{} effects", counts.effects);
    assert!(counts.markers >= 100, "{} markers", counts.markers);
    assert!(p.sequences().len() >= 4, "three sequences and a compound");

    let started = Instant::now();
    let saved = schema::save(&p);
    let save_time = started.elapsed();
    let started = Instant::now();
    let loaded = schema::load(&saved).expect("loads");
    let load_time = started.elapsed();
    assert!(loaded.content_eq(&p), "load gives back the saved tree");
    let again = schema::save(&loaded);
    assert!(saved == again, "save, load, save is byte-identical");
    assert_canonical_layout(&saved);
    eprintln!(
        "1000-clip project: {} bytes, {} lines; save {:.1} ms, load {:.1} ms (this build's profile)",
        saved.len(),
        saved.iter().filter(|b| **b == b'\n').count(),
        ms(save_time),
        ms(load_time),
    );
}

#[test]
fn compact_and_reordered_files_save_to_the_canonical_form() {
    for (name, p) in [
        ("golden", golden_project()),
        ("thousand", thousand_clips().0),
    ] {
        let canonical = schema::save(&p);
        let value: Value = serde_json::from_slice(&canonical).expect("json");
        // Compact with every object's keys sorted (struct fields too).
        let sorted = serde_json::to_vec(&value).expect("compact");
        // Compact with every object's keys reversed.
        let mut reversed = String::new();
        reversed_compact(&value, &mut reversed);
        // Pretty with a 4-space indent and CRLF line ends.
        let mut wide = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
        let mut ser = serde_json::Serializer::with_formatter(&mut wide, formatter);
        serde::Serialize::serialize(&value, &mut ser).expect("wide");
        let crlf = String::from_utf8(wide)
            .expect("utf-8")
            .replace('\n', "\r\n");
        for (form, bytes) in [
            ("sorted compact", sorted),
            ("reversed compact", reversed.into_bytes()),
            ("4-space CRLF", crlf.into_bytes()),
        ] {
            assert_ne!(bytes, canonical, "{name} {form} is a different layout");
            let resaved = schema::save(&schema::load(&bytes).expect("loads"));
            assert!(
                resaved == canonical,
                "{name}: {form} saves to the canonical bytes"
            );
        }
    }
}

#[test]
fn the_key_order_golden_is_what_save_writes() {
    let bytes = schema::save(&golden_project());
    assert_canonical_layout(&bytes);
    let path = golden_path();
    if env::var_os("GLASS_UPDATE_FIXTURES").is_some() {
        fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        fs::write(&path, &bytes).expect("write golden");
    }
    let golden = fs::read(&path).expect("read tests/fixtures/canonical/key-order.proj");
    assert!(
        golden == bytes,
        "save output differs from {}; rerun with GLASS_UPDATE_FIXTURES=1 if the change is \
         intended (it changes every project file's bytes)",
        path.display()
    );
    let text = String::from_utf8(golden).expect("utf-8");
    // Clip keyframes sort by id, not by `ClipParam` declaration order; effect params
    // and keyframes by id, not by schema declaration order.
    let keyframes = map_keys(&text, "keyframes");
    let clip = ["crop.left", "opacity", "position", "rotation", "scale"];
    assert!(keyframes.iter().any(|k| *k == clip), "{keyframes:?}");
    assert!(
        keyframes.iter().any(|k| *k == ["alpha", "zeta"]),
        "{keyframes:?}"
    );
    let params = map_keys(&text, "params");
    assert!(
        params.iter().any(|k| *k == ["alpha", "hue", "mid", "zeta"]),
        "{params:?}"
    );
    // Struct fields keep declaration order: `id` before `range` before `type`.
    let item = text.find("\"id\": ").expect("id");
    assert!(item < text.find("\"range\": ").expect("range"));
}

/// Builds a project from `seed` through a few hundred random edits, accepting whichever
/// the model allows.
fn random_project(seed: u64) -> Project {
    let mut rng = Lcg(seed);
    let p = Project::new(format!("random-{seed}"), "Random");
    let (p, a) = p.add_media("aa", "/footage/a.mov").expect("media");
    let (p, b) = p.add_media("bb", "/footage/b.mov").expect("media");
    let (mut p, s) = add_seq(p, "Main", FPS, 2, 1);
    let tracks = [s.video[0], s.video[1], s.audio[0]];
    let look = look();
    let mut items: Vec<ItemId> = Vec::new();
    let mut fx: Vec<EffectId> = Vec::new();
    for _ in 0..400 {
        let track = rng.pick(&tracks);
        let start = frames(rng.next(400) as i64, FPS);
        let range = TimeRange::new(start, frames(10 + rng.next(40) as i64, FPS));
        let item = (!items.is_empty()).then(|| rng.pick(&items));
        let result = match (rng.next(11), item) {
            (0 | 1, _) => p
                .insert_clip(
                    track,
                    ClipSpec {
                        media: rng.pick(&[a, b]),
                        range,
                        source_start: Time::seconds(2 + rng.next(20) as i64),
                    },
                )
                .map(|(n, id)| {
                    items.push(id);
                    n
                }),
            (2, Some(i)) => p.move_item(i, track, start),
            (3, Some(i)) => p.trim_item(i, range),
            (4, Some(i)) => p.set_clip_opacity(i, rng.unit()),
            (5, Some(i)) => p.add_effect(i, &look, None).map(|(n, id)| {
                fx.push(id);
                n
            }),
            (6, _) if !fx.is_empty() => {
                let id = rng.pick(&fx);
                let param = rng.pick(&["zeta", "alpha"]);
                p.set_effect_param(id, &look, param, ParamValue::Float(rng.unit()))
            }
            (7, Some(i)) => {
                let param = rng.pick(&ClipParam::ALL);
                let at = Time::seconds(rng.next(20) as i64);
                let target = KeyTarget::Clip { item: i, param };
                p.animate_param(target, at)
                    .or_else(|_| p.set_key(target, at, ParamValue::Float(rng.unit())))
            }
            (8, Some(i)) => p
                .add_marker(
                    MarkerOwner::Clip(i),
                    MarkerSpec::at(Time::seconds(rng.next(20) as i64), ColourLabel::Blue),
                )
                .map(|(n, _)| n),
            (9, Some(i)) => {
                let other = rng.pick(&items);
                p.add_transition(TransitionSpec::new(Some(i), Some(other), frames(6, FPS)))
                    .map(|(n, _)| n)
            }
            (10, Some(i)) => p.nest_items(&[i], "Nest").map(|(n, _, _)| n),
            _ => continue,
        };
        if let Ok(next) = result {
            p = next;
        }
    }
    p
}

fn random_bytes() -> Vec<u8> {
    let mut all = Vec::new();
    for seed in 1..=6 {
        all.extend(schema::save(&random_project(seed)));
    }
    all
}

#[test]
fn random_edits_save_the_same_bytes_in_another_process() {
    if let Some(out) = env::var_os(CHILD_OUT) {
        fs::write(out, random_bytes()).expect("child writes its bytes");
        return;
    }
    let clips: usize = (1..=6).map(|seed| media_clips(&random_project(seed))).sum();
    assert!(clips >= 100, "random edits kept {clips} clips");
    let here = random_bytes();
    assert!(
        here == random_bytes(),
        "two builds in one process save the same bytes"
    );
    for seed in 1..=6 {
        let bytes = schema::save(&random_project(seed));
        assert_canonical_layout(&bytes);
        assert!(bytes == schema::save(&schema::load(&bytes).expect("loads")));
    }
    // A second process has its own random hasher keys; any hash-ordered output differs.
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("canonical-child-{}.bin", std::process::id()));
    let status = Command::new(env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "random_edits_save_the_same_bytes_in_another_process",
            "--test-threads=1",
        ])
        .env(CHILD_OUT, &out)
        .status()
        .expect("run the child");
    assert!(status.success(), "child exited with {status}");
    let there = fs::read(&out).expect("child output");
    let _ = fs::remove_file(&out);
    assert!(here == there, "another process saves the same bytes");
}

#[test]
fn floats_survive_a_text_round_trip_bit_for_bit() {
    let mut rng = Lcg(0xf10a7);
    for _ in 0..200_000 {
        let x = f64::from_bits(rng.bits());
        if !x.is_finite() {
            continue;
        }
        let text = serde_json::to_string(&x).expect("f64");
        let back: f64 = serde_json::from_str(&text).expect("parse f64");
        assert_eq!(back.to_bits(), x.to_bits(), "{text}");
        let y = f32::from_bits(rng.bits() as u32);
        if !y.is_finite() {
            continue;
        }
        let text = serde_json::to_string(&y).expect("f32");
        let back: f32 = serde_json::from_str(&text).expect("parse f32");
        assert_eq!(back.to_bits(), y.to_bits(), "{text}");
        // f32 values travel through `Value` (f64) on load.
        let via: f32 = serde_json::from_value(serde_json::from_str(&text).expect("value"))
            .expect("f32 via value");
        assert_eq!(via.to_bits(), y.to_bits(), "{text} via Value");
    }
}
