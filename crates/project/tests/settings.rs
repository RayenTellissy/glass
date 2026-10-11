//! Project settings (schema v6): the format new sequences start with, the colour space and
//! the cache folder, per-sequence formats, the edits that change them with undo and redo,
//! and their refusals.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use project::autosave::autosave_location;
use project::diff::{Diff, flatten, rebuild};
use project::settings::{FRAME_RATE_PRESETS, SAMPLE_RATES};
use project::{
    Alignment, CacheFolder, ChannelLayout, ClipSpec, ColourLabel, ColourSpace, EditError,
    EditLabel, History, ItemId, MarkerOwner, MarkerSpec, Project, ProjectSettings, Rational,
    Resolution, SequenceFormat, SequenceId, Time, TimeRange, TrackKind, TransitionSpec,
    WorkingPrimaries,
};

const FPS: Rational = Rational::new(25, 1);
const NTSC: Rational = Rational::new(30000, 1001);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

struct Fixture {
    project: Project,
    seq: SequenceId,
    other: SequenceId,
    shot: ItemId,
    sound: ItemId,
}

/// "Main" at 25 fps, 48 kHz: V1 a clip at 0..2 s, A1 a clip at 0..2 s; "Other" empty.
fn fixture() -> Fixture {
    let p = Project::new("uid-settings", "Settings");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, other) = p.add_sequence("Other", FPS, 48_000).expect("other");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let spec = |range| ClipSpec {
        media,
        range,
        source_start: Time::ZERO,
    };
    let two = TimeRange::new(Time::ZERO, Time::seconds(2));
    let (p, shot) = p.insert_clip(v1, spec(two)).expect("shot");
    let (p, sound) = p.insert_clip(a1, spec(two)).expect("sound");
    Fixture {
        project: p,
        seq,
        other,
        shot,
        sound,
    }
}

/// The same nodes, element by element (small `imbl` vectors are stored inline, so
/// `Vector::ptr_eq` cannot tell).
fn same_nodes<T>(a: &imbl::Vector<Arc<T>>, b: &imbl::Vector<Arc<T>>) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Arc::ptr_eq(x, y))
}

/// Equal settings and the very same sequence nodes.
fn shares_everything(a: &Project, b: &Project) -> bool {
    a.content_eq(b) && a.settings() == b.settings() && same_nodes(a.sequences(), b.sequences())
}

fn format_of(p: &Project, seq: SequenceId) -> SequenceFormat {
    p.sequence(seq).expect("sequence").format()
}

fn custom_settings() -> ProjectSettings {
    ProjectSettings {
        sequence: SequenceFormat {
            resolution: Resolution::UHD,
            frame_rate: NTSC,
            drop_frame: true,
            sample_rate: 96_000,
            channel_layout: ChannelLayout::Surround51,
        },
        colour_space: ColourSpace::Rec2020Pq,
        cache_folder: CacheFolder::Absolute("/Volumes/Fast/cache".into()),
    }
}

#[test]
fn a_new_project_has_the_default_settings() {
    let p = Project::new("u", "New");
    let s = p.settings();
    assert_eq!(*s, ProjectSettings::default());
    assert_eq!(s.sequence.resolution, Resolution::new(1920, 1080));
    assert_eq!(s.sequence.frame_rate, Rational::new(30, 1));
    assert!(!s.sequence.drop_frame);
    assert_eq!(s.sequence.sample_rate, 48_000);
    assert_eq!(s.sequence.channel_layout, ChannelLayout::Stereo);
    assert_eq!(s.colour_space, ColourSpace::Rec709);
    assert_eq!(
        s.cache_folder,
        CacheFolder::Relative(".glass-cache".to_string())
    );
    assert_eq!(s.check(), Ok(()));
}

#[test]
fn colour_spaces_pick_the_working_primaries() {
    assert_eq!(
        ColourSpace::Rec709.working_primaries(),
        WorkingPrimaries::Bt709
    );
    assert_eq!(
        ColourSpace::Rec2020Pq.working_primaries(),
        WorkingPrimaries::Bt2020
    );
    assert_eq!(
        ColourSpace::Rec2020Hlg.working_primaries(),
        WorkingPrimaries::Bt2020
    );
    assert!(!ColourSpace::Rec709.is_hdr());
    assert!(ColourSpace::Rec2020Pq.is_hdr() && ColourSpace::Rec2020Hlg.is_hdr());
}

#[test]
fn new_sequences_take_the_project_format() {
    let p = Project::new("u", "New")
        .set_project_settings(custom_settings())
        .expect("settings");
    // `add_sequence` takes the rates given and the rest from the project.
    let (p, a) = p.add_sequence("A", FPS, 44_100).expect("a");
    assert_eq!(
        format_of(&p, a),
        SequenceFormat {
            resolution: Resolution::UHD,
            frame_rate: FPS,
            drop_frame: false,
            sample_rate: 44_100,
            channel_layout: ChannelLayout::Surround51,
        }
    );
    // `add_sequence_with` takes the whole format, the project's default here.
    let default = p.settings().sequence;
    let (p, b) = p.add_sequence_with("B", default).expect("b");
    assert_eq!(format_of(&p, b), default);
    let seq = p.sequence(b).expect("b");
    assert_eq!(seq.frame_rate, NTSC);
    assert!(seq.drop_frame);
    assert_eq!(
        seq.grid(TrackKind::Audio).expect("grid"),
        Time::new(1, 96_000).expect("time")
    );
    // And it is checked.
    let bad = SequenceFormat {
        drop_frame: true,
        frame_rate: FPS,
        ..default
    };
    assert_eq!(
        p.add_sequence_with("C", bad),
        Err(EditError::DropFrameNotAllowed(FPS))
    );
    assert_eq!(
        p.add_sequence("D", FPS, 22_050),
        Err(EditError::InvalidSampleRate(22_050))
    );
}

#[test]
fn project_settings_change_with_undo_and_leave_sequences_alone() {
    let f = fixture();
    let before = f.project.clone();
    let p = f
        .project
        .set_project_settings(custom_settings())
        .expect("settings");
    assert_eq!(*p.settings(), custom_settings());
    // Existing sequences keep their format and are shared untouched.
    assert!(same_nodes(p.sequences(), before.sequences()));
    assert_eq!(format_of(&p, f.seq).frame_rate, FPS);
    assert_eq!(format_of(&p, f.seq).resolution, Resolution::HD);

    let mut h = History::new(before.clone());
    h.commit(EditLabel::ProjectSettings, p.clone());
    assert_eq!(h.undo_label(), Some(EditLabel::ProjectSettings));
    assert_eq!(
        EditLabel::ProjectSettings.i18n_key(),
        "edit.label.project_settings"
    );
    let undone = h.undo().expect("undo").clone();
    assert_eq!(*undone.settings(), ProjectSettings::default());
    assert!(undone.content_eq(&before));
    assert_eq!(h.redo_label(), Some(EditLabel::ProjectSettings));
    let redone = h.redo().expect("redo").clone();
    assert_eq!(*redone.settings(), custom_settings());
}

#[test]
fn unchanged_settings_return_the_same_root() {
    let f = fixture();
    let same = f
        .project
        .set_project_settings(ProjectSettings::default())
        .expect("same");
    assert!(shares_everything(&same, &f.project));
    let format = format_of(&f.project, f.seq);
    let same = f.project.set_sequence_format(f.seq, format).expect("same");
    assert!(shares_everything(&same, &f.project));
    let mut h = History::new(f.project.clone());
    h.commit(EditLabel::SequenceSettings, same);
    assert!(!h.can_undo(), "a no-op leaves no undo step");
}

#[test]
fn rates_are_stored_reduced_so_an_equal_rate_in_another_form_changes_nothing() {
    let f = fixture();
    let unreduced = SequenceFormat {
        frame_rate: Rational::new(50, 2),
        ..format_of(&f.project, f.seq)
    };
    let same = f
        .project
        .set_sequence_format(f.seq, unreduced)
        .expect("same rate");
    assert!(shares_everything(&same, &f.project));
    // A real change stores the reduced form; a later equal one is again a no-op.
    let ntsc = SequenceFormat {
        frame_rate: Rational::new(60000, 2002),
        ..format_of(&f.project, f.other)
    };
    let q = f.project.set_sequence_format(f.other, ntsc).expect("ntsc");
    assert_eq!(format_of(&q, f.other).frame_rate, NTSC);
    assert!(shares_everything(
        &q.set_sequence_format(f.other, ntsc).expect("same"),
        &q
    ));
    let (q, added) = q.add_sequence_with("Fifty", unreduced).expect("add");
    assert_eq!(format_of(&q, added).frame_rate, FPS);
    let settings = ProjectSettings {
        sequence: unreduced,
        ..ProjectSettings::default()
    };
    let q = q.set_project_settings(settings.clone()).expect("settings");
    assert_eq!(q.settings().sequence.frame_rate, FPS);
    assert!(shares_everything(
        &q.set_project_settings(settings).expect("same"),
        &q
    ));
}

#[test]
fn invalid_project_settings_are_refused_in_order() {
    let p = Project::new("u", "New");
    let good = ProjectSettings::default();
    let with = |change: &dyn Fn(&mut ProjectSettings)| {
        let mut s = good.clone();
        change(&mut s);
        s
    };
    let cases: Vec<(ProjectSettings, EditError)> = vec![
        (
            with(&|s| s.sequence.resolution = Resolution::new(1921, 1080)),
            EditError::InvalidResolution {
                width: 1921,
                height: 1080,
            },
        ),
        (
            with(&|s| s.sequence.resolution = Resolution::new(1920, 0)),
            EditError::InvalidResolution {
                width: 1920,
                height: 0,
            },
        ),
        (
            with(&|s| s.sequence.resolution = Resolution::new(14, 1080)),
            EditError::InvalidResolution {
                width: 14,
                height: 1080,
            },
        ),
        (
            with(&|s| s.sequence.resolution = Resolution::new(8194, 1080)),
            EditError::InvalidResolution {
                width: 8194,
                height: 1080,
            },
        ),
        (
            with(&|s| s.sequence.frame_rate = Rational::new(0, 1)),
            EditError::InvalidFrameRate { num: 0, den: 1 },
        ),
        (
            with(&|s| s.sequence.frame_rate = Rational::new(25, 0)),
            EditError::InvalidFrameRate { num: 25, den: 0 },
        ),
        (
            with(&|s| s.sequence.frame_rate = Rational::new(-25, -1)),
            EditError::InvalidFrameRate { num: -25, den: -1 },
        ),
        (
            with(&|s| s.sequence.frame_rate = Rational::new(1, 2)),
            EditError::InvalidFrameRate { num: 1, den: 2 },
        ),
        (
            with(&|s| s.sequence.frame_rate = Rational::new(241, 1)),
            EditError::InvalidFrameRate { num: 241, den: 1 },
        ),
        (
            with(&|s| s.sequence.drop_frame = true),
            EditError::DropFrameNotAllowed(Rational::new(30, 1)),
        ),
        (
            with(&|s| s.sequence.sample_rate = 32_000),
            EditError::InvalidSampleRate(32_000),
        ),
        (
            with(&|s| s.cache_folder = CacheFolder::Relative(String::new())),
            EditError::InvalidCacheFolder(CacheFolder::Relative(String::new())),
        ),
        (
            with(&|s| s.cache_folder = CacheFolder::Absolute("cache".into())),
            EditError::InvalidCacheFolder(CacheFolder::Absolute("cache".into())),
        ),
        (
            with(&|s| s.cache_folder = CacheFolder::Relative("/tmp/cache".into())),
            EditError::InvalidCacheFolder(CacheFolder::Relative("/tmp/cache".into())),
        ),
        // The resolution is checked first, then the frame rate, drop-frame, the sample
        // rate and last the cache folder.
        (
            ProjectSettings {
                sequence: SequenceFormat {
                    resolution: Resolution::new(3, 3),
                    frame_rate: Rational::new(0, 1),
                    drop_frame: true,
                    sample_rate: 1,
                    channel_layout: ChannelLayout::Mono,
                },
                colour_space: ColourSpace::Rec709,
                cache_folder: CacheFolder::Relative(String::new()),
            },
            EditError::InvalidResolution {
                width: 3,
                height: 3,
            },
        ),
        (
            with(&|s| {
                s.sequence.frame_rate = Rational::new(0, 1);
                s.sequence.sample_rate = 1;
            }),
            EditError::InvalidFrameRate { num: 0, den: 1 },
        ),
        (
            with(&|s| {
                s.sequence.drop_frame = true;
                s.sequence.sample_rate = 1;
            }),
            EditError::DropFrameNotAllowed(Rational::new(30, 1)),
        ),
        (
            with(&|s| {
                s.sequence.sample_rate = 1;
                s.cache_folder = CacheFolder::Absolute(String::new());
            }),
            EditError::InvalidSampleRate(1),
        ),
    ];
    for (settings, error) in cases {
        assert_eq!(
            p.set_project_settings(settings.clone()),
            Err(error),
            "{settings:?}"
        );
    }
}

#[test]
fn every_preset_and_bound_is_accepted() {
    let p = Project::new("u", "New");
    for preset in FRAME_RATE_PRESETS {
        let drop = matches!(preset.name, "29.97" | "59.94");
        for drop_frame in [false, drop] {
            let mut s = ProjectSettings::default();
            s.sequence.frame_rate = preset.rate;
            s.sequence.drop_frame = drop_frame;
            p.set_project_settings(s).expect(preset.name);
        }
    }
    for rate in SAMPLE_RATES {
        let mut s = ProjectSettings::default();
        s.sequence.sample_rate = rate;
        p.set_project_settings(s).expect("sample rate");
    }
    for resolution in Resolution::PRESETS
        .into_iter()
        .chain([Resolution::new(16, 16), Resolution::new(8192, 8192)])
    {
        let mut s = ProjectSettings::default();
        s.sequence.resolution = resolution;
        p.set_project_settings(s).expect("resolution");
    }
    for rate in [
        Rational::new(1, 1),
        Rational::new(240, 1),
        Rational::new(12, 1),
    ] {
        let mut s = ProjectSettings::default();
        s.sequence.frame_rate = rate;
        p.set_project_settings(s).expect("custom rate");
    }
    for layout in ChannelLayout::ALL {
        let mut s = ProjectSettings::default();
        s.sequence.channel_layout = layout;
        p.set_project_settings(s).expect("layout");
    }
    assert_eq!(ChannelLayout::ALL.map(ChannelLayout::channels), [1, 2, 6]);
}

#[test]
fn canvas_drop_frame_and_layout_change_freely_with_undo() {
    let f = fixture();
    let format = SequenceFormat {
        resolution: Resolution::VERTICAL_HD,
        channel_layout: ChannelLayout::Mono,
        ..format_of(&f.project, f.seq)
    };
    let p = f
        .project
        .set_sequence_format(f.seq, format)
        .expect("format");
    assert_eq!(format_of(&p, f.seq), format);
    // Only that sequence changed; its tracks and the other sequence are shared.
    let (old, new) = (
        f.project.sequence(f.seq).expect("old"),
        p.sequence(f.seq).expect("new"),
    );
    assert!(same_nodes(&old.tracks, &new.tracks));
    assert!(Arc::ptr_eq(
        f.project.sequence(f.other).expect("other"),
        p.sequence(f.other).expect("other")
    ));
    assert_eq!(p.settings(), f.project.settings());

    let mut h = History::new(f.project.clone());
    h.commit(EditLabel::SequenceSettings, p.clone());
    assert_eq!(h.undo_label(), Some(EditLabel::SequenceSettings));
    assert_eq!(
        EditLabel::SequenceSettings.i18n_key(),
        "edit.label.sequence_settings"
    );
    assert_eq!(
        format_of(h.undo().expect("undo"), f.seq).resolution,
        Resolution::HD
    );
    assert_eq!(format_of(h.redo().expect("redo"), f.seq), format);

    // Drop-frame on a 29.97 sequence holding content.
    let (p, ntsc) = p.add_sequence("NTSC", NTSC, 48_000).expect("ntsc");
    let df = SequenceFormat {
        drop_frame: true,
        ..format_of(&p, ntsc)
    };
    let p = p.set_sequence_format(ntsc, df).expect("drop-frame");
    assert!(p.sequence(ntsc).expect("ntsc").drop_frame);
    // A bad format on an unknown sequence reports the format first.
    assert_eq!(
        p.set_sequence_format(
            SequenceId(9999),
            SequenceFormat {
                drop_frame: true,
                ..format
            }
        ),
        Err(EditError::DropFrameNotAllowed(FPS))
    );
    assert_eq!(
        p.set_sequence_format(SequenceId(9999), format),
        Err(EditError::UnknownSequence(SequenceId(9999)))
    );
}

#[test]
fn a_rate_change_that_keeps_everything_on_the_grid_is_allowed() {
    let f = fixture();
    // 25 → 50 fps: every 1/25 s boundary is a 1/50 s one. 48 → 96 kHz likewise.
    let format = SequenceFormat {
        frame_rate: Rational::new(50, 1),
        sample_rate: 96_000,
        ..format_of(&f.project, f.seq)
    };
    let p = f
        .project
        .set_sequence_format(f.seq, format)
        .expect("format");
    let seq = p.sequence(f.seq).expect("seq");
    assert_eq!(seq.frame_rate, Rational::new(50, 1));
    assert_eq!(seq.sample_rate, 96_000);
    // Nothing moved.
    for item in [f.shot, f.sound] {
        assert_eq!(
            p.find_item(item).expect("item").2.range,
            f.project.find_item(item).expect("item").2.range
        );
    }
    p.validate().expect("valid");
    // 2 s is 59.94 frames at 29.97 fps: off the grid, so the change is refused.
    let to_ntsc = SequenceFormat {
        frame_rate: NTSC,
        ..format_of(&f.project, f.seq)
    };
    assert_eq!(
        f.project.set_sequence_format(f.seq, to_ntsc),
        Err(EditError::OffGrid {
            time: Time::seconds(2),
            grid: NTSC.frame_duration().expect("grid"),
            kind: TrackKind::Video,
        })
    );
    // The empty sequence takes any rate.
    let p = f
        .project
        .set_sequence_format(f.other, to_ntsc)
        .expect("empty");
    assert_eq!(p.sequence(f.other).expect("other").frame_rate, NTSC);
}

#[test]
fn a_sample_rate_change_is_refused_when_audio_falls_off_the_grid() {
    let f = fixture();
    let one_sample = Time::new(1, 48_000).expect("time");
    let p = f
        .project
        .trim_item(
            f.sound,
            TimeRange::new(
                one_sample,
                Time::seconds(2).checked_sub(one_sample).expect("t"),
            ),
        )
        .expect("trim");
    let format = SequenceFormat {
        sample_rate: 44_100,
        ..format_of(&p, f.seq)
    };
    assert_eq!(
        p.set_sequence_format(f.seq, format),
        Err(EditError::OffGrid {
            time: one_sample,
            grid: Time::new(1, 44_100).expect("grid"),
            kind: TrackKind::Audio,
        })
    );
    // 96 kHz holds every 48 kHz boundary.
    let up = SequenceFormat {
        sample_rate: 96_000,
        ..format_of(&p, f.seq)
    };
    p.set_sequence_format(f.seq, up).expect("96 kHz");
}

#[test]
fn sequence_markers_must_stay_on_the_frame_grid_and_items_are_checked_first() {
    let f = fixture();
    let (p, _) = f
        .project
        .add_marker(
            MarkerOwner::Sequence(f.seq),
            MarkerSpec::at(frames(1), ColourLabel::Blue),
        )
        .expect("marker");
    let to_24 = SequenceFormat {
        frame_rate: Rational::new(24, 1),
        ..format_of(&p, f.seq)
    };
    // Items at whole seconds fit 24 fps; the marker at 1/25 s does not.
    assert_eq!(
        p.set_sequence_format(f.seq, to_24),
        Err(EditError::MarkerOffGrid {
            time: frames(1),
            grid: Time::new(1, 24).expect("grid"),
        })
    );
    // With an item off the 24 fps grid too, the item is reported first.
    let p = p
        .trim_item(f.shot, TimeRange::new(Time::ZERO, frames(49)))
        .expect("trim");
    assert_eq!(
        p.set_sequence_format(f.seq, to_24),
        Err(EditError::OffGrid {
            time: frames(49),
            grid: Time::new(1, 24).expect("grid"),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn a_rate_change_that_moves_a_centred_transition_past_its_handle_is_refused() {
    let p = Project::new("u", "T");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let spec = |start, source_start| ClipSpec {
        media,
        range: TimeRange::new(start, Time::seconds(1)),
        source_start,
    };
    let (p, a) = p.insert_clip(v1, spec(Time::ZERO, Time::ZERO)).expect("a");
    // One frame of head handle on the incoming clip.
    let (p, b) = p
        .insert_clip(v1, spec(Time::seconds(1), frames(1)))
        .expect("b");
    // Three frames centred: one before the cut, two after.
    let mut t = TransitionSpec::new(Some(a), Some(b), frames(3));
    t.alignment = Alignment::Centre;
    let (p, _) = p.add_transition(t).expect("transition");
    // At 50 fps the three frames are six steps, three before the cut: past the handle.
    let to_50 = SequenceFormat {
        frame_rate: Rational::new(50, 1),
        ..format_of(&p, seq)
    };
    assert_eq!(
        p.set_sequence_format(seq, to_50),
        Err(EditError::TransitionHandle {
            item: b,
            needed: Time::new(3, 50).expect("t"),
            available: frames(1),
        })
    );
    // A transition length off the new grid is refused as off grid.
    let to_24 = SequenceFormat {
        frame_rate: Rational::new(24, 1),
        ..format_of(&p, seq)
    };
    assert_eq!(
        p.set_sequence_format(seq, to_24),
        Err(EditError::OffGrid {
            time: frames(3),
            grid: Time::new(1, 24).expect("grid"),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn settings_and_formats_travel_in_autosave_diffs() {
    let f = fixture();
    let base = flatten(&f.project);
    let p = f
        .project
        .set_project_settings(custom_settings())
        .expect("settings");
    let p = p
        .set_sequence_format(
            f.seq,
            SequenceFormat {
                resolution: Resolution::SQUARE,
                ..format_of(&p, f.seq)
            },
        )
        .expect("format");
    let next = flatten(&p);
    let diff = Diff::between(&base, &next);
    // The root record (settings) and the one sequence record, nothing else.
    assert_eq!(diff.upserts.len(), 2, "{diff:?}");
    assert!(diff.removals.is_empty());
    let mut applied = base.clone();
    diff.apply(&mut applied);
    let rebuilt = rebuild(&applied).expect("rebuild");
    assert!(rebuilt.content_eq(&p));
    assert_eq!(*rebuilt.settings(), custom_settings());
}

#[test]
fn invalid_settings_or_formats_in_a_file_are_refused_on_load() {
    let f = fixture();
    let bytes = project::schema::save(&f.project);
    let cases = [
        (
            "/project/settings/sequence/sample_rate",
            serde_json::json!(22050),
        ),
        ("/project/settings/colour_space", serde_json::json!("p3")),
        (
            "/project/settings/cache_folder",
            serde_json::json!({ "relative": "" }),
        ),
        (
            "/project/sequences/0/resolution",
            serde_json::json!({ "width": 1919, "height": 1080 }),
        ),
        ("/project/sequences/0/drop_frame", serde_json::json!(true)),
        (
            "/project/sequences/0/frame_rate",
            serde_json::json!({ "num": 1000, "den": 1 }),
        ),
        (
            "/project/sequences/0/channel_layout",
            serde_json::json!("7.1"),
        ),
    ];
    for (pointer, value) in cases {
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        *json.pointer_mut(pointer).expect(pointer) = value;
        let err =
            project::schema::load(&serde_json::to_vec(&json).expect("JSON")).expect_err(pointer);
        assert_eq!(err.i18n_key(), "project.open_error.damaged", "{pointer}");
    }
}

#[test]
fn autosave_goes_to_the_project_cache_folder_once_the_project_has_a_file() {
    let user = Path::new("/Users/me/Library/Caches/Glass");
    let p = Project::new("uid-7", "Harbour");
    // Unsaved: the per-user cache folder.
    assert_eq!(
        autosave_location(&p, None, user),
        user.join("autosave").join("uid-7")
    );
    // Saved: `.glass-cache/` beside the file.
    let file = Path::new("/films/harbour/Harbour.glass");
    assert_eq!(
        autosave_location(&p, Some(file), user),
        PathBuf::from("/films/harbour/.glass-cache/autosave/uid-7")
    );
    // A relative cache folder elsewhere, and an absolute one.
    let mut s = ProjectSettings {
        cache_folder: CacheFolder::Relative("../caches/harbour".into()),
        ..ProjectSettings::default()
    };
    let rel = p.set_project_settings(s.clone()).expect("relative");
    assert_eq!(
        autosave_location(&rel, Some(file), user),
        Path::new("/films/harbour")
            .join("..")
            .join("caches")
            .join("harbour")
            .join("autosave")
            .join("uid-7")
    );
    s.cache_folder = CacheFolder::Absolute("/Volumes/Fast/cache".into());
    let abs = p.set_project_settings(s).expect("absolute");
    assert_eq!(
        autosave_location(&abs, Some(file), user),
        PathBuf::from("/Volumes/Fast/cache/autosave/uid-7")
    );
    // Still unsaved: the per-user cache, whatever the setting.
    assert_eq!(
        autosave_location(&abs, None, user),
        user.join("autosave").join("uid-7")
    );
}
