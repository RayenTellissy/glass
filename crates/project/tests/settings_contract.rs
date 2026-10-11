//! Project settings checked against their contract (schema v6, Decision 55): the bounds of
//! each format field, the order refusals are reported in when several things are wrong,
//! the cache folder policy, the grid checks of a rate change, autosave records, and
//! migrating v5 files.

use std::path::{Path, PathBuf};

use project::diff::{Diff, ROOT_ID, flatten, rebuild};
use project::schema::{self, SCHEMA_VERSION};
use project::{
    Alignment, CacheFolder, ChannelLayout, ClipSpec, ColourLabel, ColourSpace, EditError, ItemId,
    LoadError, MarkerOwner, MarkerSpec, MediaId, NodeId, Project, ProjectSettings, Rational,
    Resolution, SequenceFormat, SequenceId, Time, TimeRange, TrackId, TrackKind, TransitionSpec,
};
use serde_json::{Value, json};

const FPS: Rational = Rational::new(25, 1);
/// 1/10 s holds every fifth 1/25 s boundary: 5 frames at 25 fps stay on its grid, 3 or 4
/// do not.
const TEN: Rational = Rational::new(10, 1);
const NTSC: Rational = Rational::new(30000, 1001);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn tenths(n: i64) -> Time {
    Time::new(n, 10).expect("time")
}

fn ten_grid() -> Time {
    TEN.frame_duration().expect("grid")
}

fn format(frame_rate: Rational, drop_frame: bool) -> SequenceFormat {
    SequenceFormat {
        frame_rate,
        drop_frame,
        ..SequenceFormat::default()
    }
}

/// Adds a sequence in `format` to a new project, the boundary a new sequence goes through.
fn add_with(format: SequenceFormat) -> Result<(), EditError> {
    Project::new("u", "Formats")
        .add_sequence_with("S", format)
        .map(|_| ())
}

fn settings_with_cache(folder: CacheFolder) -> ProjectSettings {
    ProjectSettings {
        cache_folder: folder,
        ..ProjectSettings::default()
    }
}

/// A sequence at 25 fps, 48 kHz with one media file to cut from.
struct Timeline {
    project: Project,
    media: MediaId,
    seq: SequenceId,
}

impl Timeline {
    fn new() -> Self {
        let p = Project::new("uid-contract", "Contract");
        let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
        let (project, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
        Self {
            project,
            media,
            seq,
        }
    }

    /// A track at `index` (0 is the top), or at the bottom.
    fn track(&mut self, kind: TrackKind, index: Option<usize>) -> TrackId {
        let (p, track) = self
            .project
            .add_track(self.seq, kind, "T", index)
            .expect("track");
        self.project = p;
        track
    }

    fn clip(&mut self, track: TrackId, start: Time, length: Time, handle: Time) -> ItemId {
        let spec = ClipSpec {
            media: self.media,
            range: TimeRange::new(start, length),
            source_start: handle,
        };
        let (p, item) = self.project.insert_clip(track, spec).expect("clip");
        self.project = p;
        item
    }

    /// A centred transition of `length` on the cut between two clips.
    fn transition(&mut self, outgoing: ItemId, incoming: ItemId, length: Time) {
        let mut spec = TransitionSpec::new(Some(outgoing), Some(incoming), length);
        spec.alignment = Alignment::Centre;
        let (p, _) = self.project.add_transition(spec).expect("transition");
        self.project = p;
    }

    fn marker(&mut self, at: Time) {
        let (p, _) = self
            .project
            .add_marker(
                MarkerOwner::Sequence(self.seq),
                MarkerSpec::at(at, ColourLabel::Blue),
            )
            .expect("marker");
        self.project = p;
    }

    fn change(&self, frame_rate: Rational, sample_rate: u32) -> Result<Project, EditError> {
        let current = self.project.sequence(self.seq).expect("seq").format();
        self.project.set_sequence_format(
            self.seq,
            SequenceFormat {
                frame_rate,
                sample_rate,
                ..current
            },
        )
    }
}

#[test]
fn resolutions_are_even_and_from_16_to_8192_on_both_sides() {
    let accepted = [(16, 16), (8192, 8192), (16, 8192), (8192, 16), (18, 18)];
    for (width, height) in accepted {
        let resolution = Resolution::new(width, height);
        let f = SequenceFormat {
            resolution,
            ..SequenceFormat::default()
        };
        assert_eq!(add_with(f), Ok(()), "{width} x {height}");
    }
    let refused = [
        (15, 1080),
        (1920, 15),
        (14, 1080),
        (1920, 14),
        (17, 1080),
        (1920, 1081),
        (8193, 1080),
        (1920, 8193),
        (8194, 1080),
        (1920, 8194),
        (0, 0),
        (u32::MAX - 1, 1080),
        (u32::MAX, 1080),
    ];
    for (width, height) in refused {
        let f = SequenceFormat {
            resolution: Resolution::new(width, height),
            ..SequenceFormat::default()
        };
        assert_eq!(
            add_with(f),
            Err(EditError::InvalidResolution { width, height }),
            "{width} x {height}"
        );
    }
}

#[test]
fn frame_rates_run_from_exactly_1_to_exactly_240_fps_in_any_form() {
    let accepted = [
        (1, 1),
        (1001, 1001),
        (240, 1),
        (480, 2),
        (240_000, 1000),
        (24000, 1001),
        (239_999, 1000),
    ];
    for (num, den) in accepted {
        assert_eq!(
            add_with(format(Rational::new(num, den), false)),
            Ok(()),
            "{num}/{den}"
        );
    }
    let refused = [
        (999, 1000),
        (1000, 1001),
        (481, 2),
        (240_001, 1000),
        (241, 1),
        (0, 1),
        (0, 0),
        (-25, 1),
        (25, -1),
        (i32::MAX, 1),
        (1, i32::MAX),
    ];
    for (num, den) in refused {
        assert_eq!(
            add_with(format(Rational::new(num, den), false)),
            Err(EditError::InvalidFrameRate { num, den }),
            "{num}/{den}"
        );
    }
}

#[test]
fn drop_frame_is_allowed_only_at_rates_equal_to_29_97_or_59_94() {
    let allowed = [
        (30000, 1001),
        (60000, 1001),
        (60000, 2002),
        (90000, 3003),
        (120_000, 2002),
    ];
    for (num, den) in allowed {
        assert_eq!(
            add_with(format(Rational::new(num, den), true)),
            Ok(()),
            "{num}/{den}"
        );
    }
    // Close is not equal: 2997/100 is 29.97 exactly, 30000/1001 is 29.97002997...
    let refused = [
        (24000, 1001),
        (30, 1),
        (60, 1),
        (25, 1),
        (30000, 1000),
        (2997, 100),
        (5994, 100),
        (120_000, 1001),
    ];
    for (num, den) in refused {
        let rate = Rational::new(num, den);
        assert_eq!(
            add_with(format(rate, true)),
            Err(EditError::DropFrameNotAllowed(rate)),
            "{num}/{den}"
        );
        // Without drop-frame every one of them is a usable rate.
        assert_eq!(add_with(format(rate, false)), Ok(()), "{num}/{den}");
    }
}

#[test]
fn only_44_1_48_and_96_khz_are_sample_rates() {
    for rate in [44_100, 48_000, 96_000] {
        let f = SequenceFormat {
            sample_rate: rate,
            ..SequenceFormat::default()
        };
        assert_eq!(add_with(f), Ok(()), "{rate}");
    }
    for rate in [0, 22_050, 47_999, 48_001, 88_200, 192_000, u32::MAX] {
        let f = SequenceFormat {
            sample_rate: rate,
            ..SequenceFormat::default()
        };
        assert_eq!(
            add_with(f),
            Err(EditError::InvalidSampleRate(rate)),
            "{rate}"
        );
    }
}

#[test]
fn of_any_two_bad_settings_the_one_checked_first_is_reported() {
    // The contract's order: resolution, frame rate, drop-frame, sample rate, cache folder.
    type Fault = (fn(&mut ProjectSettings), EditError);
    let faults: [Fault; 5] = [
        (
            |s| s.sequence.resolution = Resolution::new(15, 1080),
            EditError::InvalidResolution {
                width: 15,
                height: 1080,
            },
        ),
        (
            |s| s.sequence.frame_rate = Rational::new(241, 1),
            EditError::InvalidFrameRate { num: 241, den: 1 },
        ),
        (
            |s| s.sequence.drop_frame = true,
            EditError::DropFrameNotAllowed(Rational::new(30, 1)),
        ),
        (
            |s| s.sequence.sample_rate = 22_050,
            EditError::InvalidSampleRate(22_050),
        ),
        (
            |s| s.cache_folder = CacheFolder::Absolute("cache".into()),
            EditError::InvalidCacheFolder(CacheFolder::Absolute("cache".into())),
        ),
    ];
    let p = Project::new("u", "Order");
    for (i, (first, expected)) in faults.iter().enumerate() {
        for (later, _) in &faults[i + 1..] {
            // Applied in both orders, so the result cannot depend on which came last.
            for flip in [false, true] {
                let mut s = ProjectSettings::default();
                if flip {
                    later(&mut s);
                    first(&mut s);
                } else {
                    first(&mut s);
                    later(&mut s);
                }
                assert_eq!(
                    p.set_project_settings(s.clone()),
                    Err(expected.clone()),
                    "{s:?}"
                );
            }
        }
    }
}

#[test]
fn cache_folder_kinds_must_match_whether_the_text_is_rooted() {
    let p = Project::new("u", "Cache");
    let accepted = [
        CacheFolder::Relative(".".into()),
        CacheFolder::Relative("..\\shared".into()),
        CacheFolder::Relative("a/b\\c".into()),
        CacheFolder::Relative("caché".into()),
        CacheFolder::Relative("cache:1".into()),
        CacheFolder::Absolute("/".into()),
        CacheFolder::Absolute("\\cache".into()),
        CacheFolder::Absolute("C:".into()),
        CacheFolder::Absolute("c:\\cache".into()),
        CacheFolder::Absolute("z:/cache".into()),
    ];
    for folder in accepted {
        let next = p
            .set_project_settings(settings_with_cache(folder.clone()))
            .unwrap_or_else(|e| panic!("{folder:?}: {e}"));
        assert_eq!(next.settings().cache_folder, folder);
    }
    let refused = [
        CacheFolder::Relative(String::new()),
        CacheFolder::Absolute(String::new()),
        CacheFolder::Relative(" \t\n".into()),
        CacheFolder::Absolute("   ".into()),
        CacheFolder::Relative("\0".into()),
        CacheFolder::Relative("cache\0".into()),
        CacheFolder::Absolute("C:\\a\0b".into()),
        CacheFolder::Relative("/".into()),
        CacheFolder::Relative("\\cache".into()),
        CacheFolder::Relative("\\\\server\\share".into()),
        CacheFolder::Relative("C:".into()),
        CacheFolder::Relative("c:cache".into()),
        CacheFolder::Absolute("./cache".into()),
        CacheFolder::Absolute("..\\cache".into()),
        CacheFolder::Absolute("~/cache".into()),
    ];
    for folder in refused {
        assert_eq!(
            p.set_project_settings(settings_with_cache(folder.clone())),
            Err(EditError::InvalidCacheFolder(folder.clone())),
            "{folder:?}"
        );
    }
}

#[test]
fn cache_folders_resolve_by_path_arithmetic_on_either_separator() {
    let file = Path::new("/films/harbour/Harbour.glass");
    let films = Path::new("/films/harbour");
    assert_eq!(
        CacheFolder::Relative("a\\b/c".into()).resolve(Some(file)),
        Some(films.join("a").join("b").join("c"))
    );
    assert_eq!(
        CacheFolder::Relative("..\\shared/cache".into()).resolve(Some(file)),
        Some(films.join("..").join("shared").join("cache"))
    );
    // A file named without a folder: the cache is relative to wherever the name is.
    assert_eq!(
        CacheFolder::default().resolve(Some(Path::new("Harbour.glass"))),
        Some(PathBuf::from(".glass-cache"))
    );
    // Relative without a file: nothing to resolve against.
    assert_eq!(CacheFolder::Relative("a\\b".into()).resolve(None), None);
    // Absolute folders are taken as written, separators included.
    let windows = CacheFolder::Absolute("D:\\cache\\glass".into());
    assert_eq!(
        windows.resolve(Some(file)),
        Some(PathBuf::from("D:\\cache\\glass"))
    );
    assert_eq!(
        windows.resolve(None),
        Some(PathBuf::from("D:\\cache\\glass"))
    );
}

#[test]
fn new_sequences_never_inherit_drop_frame_through_add_sequence() {
    let p = Project::new("u", "Drop")
        .set_project_settings(ProjectSettings {
            sequence: SequenceFormat {
                resolution: Resolution::DCI_4K,
                frame_rate: NTSC,
                drop_frame: true,
                sample_rate: 48_000,
                channel_layout: ChannelLayout::Mono,
            },
            ..ProjectSettings::default()
        })
        .expect("settings");
    // Even at the project's own drop-frame rate.
    let (p, seq) = p.add_sequence("NTSC", NTSC, 96_000).expect("seq");
    assert_eq!(
        p.sequence(seq).expect("seq").format(),
        SequenceFormat {
            resolution: Resolution::DCI_4K,
            frame_rate: NTSC,
            drop_frame: false,
            sample_rate: 96_000,
            channel_layout: ChannelLayout::Mono,
        }
    );
}

#[test]
fn a_bad_format_is_refused_before_any_grid_check() {
    let mut t = Timeline::new();
    let v1 = t.track(TrackKind::Video, None);
    t.clip(v1, Time::ZERO, frames(3), Time::ZERO);
    // 10 fps puts the 3-frame clip off the grid, but the sample rate is reported first.
    assert_eq!(
        t.change(TEN, 22_050),
        Err(EditError::InvalidSampleRate(22_050))
    );
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::OffGrid {
            time: frames(3),
            grid: ten_grid(),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn rate_change_refusals_go_top_track_first_whatever_the_kinds() {
    let mut t = Timeline::new();
    // Added first, ends up at the bottom: a video clip 3 frames long.
    let lower = t.track(TrackKind::Video, None);
    t.clip(lower, Time::ZERO, frames(3), Time::ZERO);
    // Added second at the top: an audio clip one 48 kHz sample long.
    let upper = t.track(TrackKind::Audio, Some(0));
    let one_sample = Time::new(1, 48_000).expect("time");
    t.clip(upper, Time::ZERO, Time::seconds(1), Time::ZERO);
    t.clip(upper, Time::seconds(2), one_sample, Time::ZERO);
    // And a marker off every new grid.
    t.marker(frames(1));
    assert_eq!(
        t.change(TEN, 44_100),
        Err(EditError::OffGrid {
            time: one_sample,
            grid: Time::new(1, 44_100).expect("grid"),
            kind: TrackKind::Audio,
        })
    );
    // At 48 kHz the audio stays on its grid and the video below is reported.
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::OffGrid {
            time: frames(3),
            grid: ten_grid(),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn a_track_reports_its_earliest_item_before_any_of_its_transitions() {
    let mut t = Timeline::new();
    let v1 = t.track(TrackKind::Video, None);
    let a = t.clip(v1, Time::ZERO, Time::seconds(1), Time::ZERO);
    let b = t.clip(v1, Time::seconds(1), Time::seconds(1), Time::seconds(1));
    // A 3-frame transition off the 10 fps grid, on the earliest cut.
    t.transition(a, b, frames(3));
    // Two items off the grid, the later one placed first.
    t.clip(v1, Time::seconds(5), frames(7), Time::ZERO);
    t.clip(v1, Time::seconds(3), frames(4), Time::ZERO);
    t.marker(frames(1));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::OffGrid {
            time: frames(4),
            grid: ten_grid(),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn a_transition_on_a_higher_track_is_reported_before_items_below_it() {
    let mut t = Timeline::new();
    let lower = t.track(TrackKind::Video, None);
    t.clip(lower, Time::ZERO, frames(7), Time::ZERO);
    let upper = t.track(TrackKind::Video, Some(0));
    let a = t.clip(upper, Time::ZERO, Time::seconds(1), Time::ZERO);
    let b = t.clip(upper, Time::seconds(1), Time::seconds(1), Time::seconds(1));
    t.transition(a, b, frames(3));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::OffGrid {
            time: frames(3),
            grid: ten_grid(),
            kind: TrackKind::Video,
        })
    );
}

/// V1 holds clips at 0, 1, 2 and 3 s, one second each. The clip at 1 s has a head handle
/// of 2 frames, as does the clip at 3 s when `tight_late_handle`, otherwise a second.
fn four_clips(t: &mut Timeline, tight_late_handle: bool) -> [ItemId; 4] {
    let v1 = t.track(TrackKind::Video, None);
    let late = if tight_late_handle {
        frames(2)
    } else {
        Time::seconds(1)
    };
    [
        t.clip(v1, Time::ZERO, Time::seconds(1), Time::ZERO),
        t.clip(v1, Time::seconds(1), Time::seconds(1), frames(2)),
        t.clip(v1, Time::seconds(2), Time::seconds(1), Time::seconds(1)),
        t.clip(v1, Time::seconds(3), Time::seconds(1), late),
    ]
}

#[test]
fn transition_lengths_are_all_checked_before_any_fit() {
    let mut t = Timeline::new();
    let [a, b, c, d] = four_clips(&mut t, false);
    // Lower id, earlier cut: 5 frames stay on the 10 fps grid, but centred they need
    // 1/10 s before the cut where the incoming clip has 2/25 s of handle.
    t.transition(a, b, frames(5));
    // Higher id, later cut: 3 frames are off the 10 fps grid.
    t.transition(c, d, frames(3));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::OffGrid {
            time: frames(3),
            grid: ten_grid(),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn transition_lengths_are_checked_in_id_order() {
    let mut t = Timeline::new();
    let [a, b, c, d] = four_clips(&mut t, false);
    // The later cut gets the lower id.
    t.transition(c, d, frames(3));
    t.transition(a, b, frames(4));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::OffGrid {
            time: frames(3),
            grid: ten_grid(),
            kind: TrackKind::Video,
        })
    );
}

#[test]
fn transition_fits_are_checked_in_cut_order() {
    let mut t = Timeline::new();
    let [a, b, c, d] = four_clips(&mut t, true);
    // Both no longer fit their incoming clip's handle at 10 fps; the later cut has the
    // lower id.
    t.transition(c, d, frames(5));
    t.transition(a, b, frames(5));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::TransitionHandle {
            item: b,
            needed: tenths(1),
            available: frames(2),
        })
    );
}

#[test]
fn transitions_are_reported_before_markers_and_markers_in_time_order() {
    let mut t = Timeline::new();
    let [a, b, _, _] = four_clips(&mut t, false);
    // Markers off the 10 fps grid, the later one added first.
    t.marker(frames(9));
    t.marker(frames(3));
    t.transition(a, b, frames(5));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::TransitionHandle {
            item: b,
            needed: tenths(1),
            available: frames(2),
        })
    );
    // Without the transition, the earliest marker.
    let mut t = Timeline::new();
    four_clips(&mut t, false);
    t.marker(frames(9));
    t.marker(frames(3));
    assert_eq!(
        t.change(TEN, 48_000),
        Err(EditError::MarkerOffGrid {
            time: frames(3),
            grid: ten_grid(),
        })
    );
}

fn upsert_ids(diff: &Diff) -> Vec<NodeId> {
    diff.upserts.iter().map(|n| n.id()).collect()
}

#[test]
fn autosave_records_one_root_and_one_record_per_changed_sequence() {
    let mut t = Timeline::new();
    let v1 = t.track(TrackKind::Video, None);
    t.clip(v1, Time::ZERO, Time::seconds(2), Time::ZERO);
    let main = t.seq;
    let (p, _second) = t.project.add_sequence("Second", FPS, 48_000).expect("2");
    let (p, third) = p.add_sequence("Third", FPS, 48_000).expect("3");
    let base = flatten(&p);

    // Project settings only, the format for new sequences included: the root record,
    // no sequence.
    let settings = ProjectSettings {
        sequence: SequenceFormat {
            resolution: Resolution::UHD,
            channel_layout: ChannelLayout::Mono,
            ..SequenceFormat::default()
        },
        colour_space: ColourSpace::Rec2020Pq,
        cache_folder: CacheFolder::Absolute("/Volumes/Fast".into()),
    };
    let only_settings = p.set_project_settings(settings).expect("settings");
    let diff = Diff::between(&base, &flatten(&only_settings));
    assert_eq!(upsert_ids(&diff), [ROOT_ID]);
    assert!(diff.removals.is_empty());

    // A rate change that keeps everything on the grid: the sequence record only, its
    // items untouched.
    let rate = SequenceFormat {
        frame_rate: Rational::new(50, 1),
        ..p.sequence(main).expect("main").format()
    };
    let regridded = p.set_sequence_format(main, rate).expect("rate");
    let diff = Diff::between(&base, &flatten(&regridded));
    assert_eq!(upsert_ids(&diff), [main.node()]);

    // Settings and two of the three sequences.
    let both = only_settings
        .set_sequence_format(main, rate)
        .expect("main")
        .set_sequence_format(
            third,
            SequenceFormat {
                channel_layout: ChannelLayout::Surround51,
                ..p.sequence(third).expect("third").format()
            },
        )
        .expect("third");
    let diff = Diff::between(&base, &flatten(&both));
    assert_eq!(upsert_ids(&diff), [ROOT_ID, main.node(), third.node()]);
    let mut applied = base.clone();
    diff.apply(&mut applied);
    assert!(rebuild(&applied).expect("rebuild").content_eq(&both));
}

#[test]
fn settings_serialise_with_their_documented_names() {
    let names = [
        (ColourSpace::Rec709, "rec709"),
        (ColourSpace::Rec2020Pq, "rec2020_pq"),
        (ColourSpace::Rec2020Hlg, "rec2020_hlg"),
    ];
    for (space, name) in names {
        assert_eq!(serde_json::to_value(space).expect("JSON"), json!(name));
    }
    let layouts = [
        (ChannelLayout::Mono, "mono"),
        (ChannelLayout::Stereo, "stereo"),
        (ChannelLayout::Surround51, "surround51"),
    ];
    for (layout, name) in layouts {
        assert_eq!(serde_json::to_value(layout).expect("JSON"), json!(name));
    }
    assert_eq!(
        serde_json::to_value(CacheFolder::default()).expect("JSON"),
        json!({ "relative": ".glass-cache" })
    );
    assert_eq!(
        serde_json::to_value(CacheFolder::Absolute("D:\\cache".into())).expect("JSON"),
        json!({ "absolute": "D:\\cache" })
    );
}

fn v5_basic() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/projects/v5-basic.json");
    serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("JSON")
}

#[test]
fn a_v5_file_that_already_has_settings_or_a_sequence_format_is_refused() {
    let intact = serde_json::to_vec(&v5_basic()).expect("JSON");
    schema::load(&intact).expect("the fixture itself loads");
    let cases: [(&str, Value); 4] = [
        (
            "/project/settings",
            serde_json::to_value(ProjectSettings::default()).expect("JSON"),
        ),
        (
            "/project/sequences/0/resolution",
            json!({ "width": 1920, "height": 1080 }),
        ),
        ("/project/sequences/0/drop_frame", json!(false)),
        ("/project/sequences/0/channel_layout", json!("stereo")),
    ];
    for (pointer, value) in cases {
        let mut file = v5_basic();
        let (parent, key) = pointer.rsplit_once('/').expect("pointer");
        file.pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .expect(parent)
            .insert(key.to_string(), value);
        let err = schema::load(&serde_json::to_vec(&file).expect("JSON")).expect_err(pointer);
        assert!(
            matches!(err, LoadError::Migration { from: 5, .. }),
            "{pointer}: {err:?}"
        );
    }
}

#[test]
fn a_v5_ntsc_sequence_keeps_its_rate_without_drop_frame() {
    let mut file = v5_basic();
    file["project"]["sequences"][0]["frame_rate"] = json!({ "num": 30000, "den": 1001 });
    // The fixture's items sit on whole seconds at 25 fps; drop them so the rate is valid.
    for track in file["project"]["sequences"][0]["tracks"]
        .as_array_mut()
        .expect("tracks")
    {
        track["items"] = json!([]);
        track["transitions"] = json!([]);
    }
    let next_id = file["project"]["next_id"].clone();
    let p = schema::load(&serde_json::to_vec(&file).expect("JSON")).expect("loads");
    let seq = &p.sequences()[0];
    assert_eq!(seq.frame_rate, NTSC);
    assert!(!seq.drop_frame);
    assert_eq!(seq.resolution, Resolution::HD);
    assert_eq!(seq.channel_layout, ChannelLayout::Stereo);
    assert_eq!(json!(p.next_id()), next_id);
    assert_eq!(*p.settings(), ProjectSettings::default());
    // Saved again, it is a current file.
    let saved: Value = serde_json::from_slice(&schema::save(&p)).expect("JSON");
    assert_eq!(saved["schema_version"], json!(SCHEMA_VERSION));
}
