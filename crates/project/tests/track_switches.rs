//! Track switches (schema v9): mute, solo, visibility, lock and height as project edits
//! with undo labels, the switches each track kind has, solo and mute deciding what is
//! heard, the lock refusing every edit to its track and what is on it (and nothing
//! else), and validation of switches in files. An edit that changes nothing returns an
//! equal root.

use project::history::{EditLabel, History};
use project::params::effects;
use project::schema::{self, LoadError};
use project::validate::ValidationError;
use project::{
    ClipParam, ClipSource, ClipSpec, ColourLabel, EditError, ItemId, KeyTarget, MarkerOwner,
    MarkerSpec, MediaId, Project, Rational, Relink, SequenceId, Time, TimeRange, TrackHeight,
    TrackId, TrackKind, TransitionSpec,
};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn range(start: i64, len: i64) -> TimeRange {
    TimeRange::new(frames(start), frames(len))
}

struct Fixture {
    project: Project,
    media: MediaId,
    seq: SequenceId,
    v2: TrackId,
    v1: TrackId,
    a1: TrackId,
    a2: TrackId,
    s1: TrackId,
    /// V1: 0..50 and 50..100 with a dissolve on the cut.
    c1: ItemId,
    c2: ItemId,
    /// V2: 20..40.
    top: ItemId,
    /// A1: 0..2 s.
    sound: ItemId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-switches", "Switches");
    let (p, media) = p.add_media("00ff", "/media/a.mov").expect("media");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, s1) = p
        .add_track(seq, TrackKind::Subtitle, "S1", None)
        .expect("s1");
    let (p, v2) = p.add_track(seq, TrackKind::Video, "V2", None).expect("v2");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, a1) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let (p, a2) = p.add_track(seq, TrackKind::Audio, "A2", None).expect("a2");
    let clip = |start: i64, len: i64, source: i64| ClipSpec {
        media,
        range: range(start, len),
        source_start: frames(source),
    };
    let (p, c1) = p.insert_clip(v1, clip(0, 50, 0)).expect("c1");
    let (p, c2) = p.insert_clip(v1, clip(50, 50, 100)).expect("c2");
    let (p, top) = p.insert_clip(v2, clip(20, 20, 10)).expect("top");
    let (p, sound) = p
        .insert_clip(
            a1,
            ClipSpec {
                media,
                range: TimeRange::new(Time::ZERO, Time::seconds(2)),
                source_start: Time::ZERO,
            },
        )
        .expect("sound");
    let (p, _) = p
        .add_transition(TransitionSpec::new(Some(c1), Some(c2), frames(10)))
        .expect("dissolve");
    Fixture {
        project: p,
        media,
        seq,
        v2,
        v1,
        a1,
        a2,
        s1,
        c1,
        c2,
        top,
        sound,
    }
}

/// The same nodes, element by element (small `imbl` vectors are stored inline, so their
/// own `ptr_eq` is false for copies).
fn shared<T>(a: &imbl::Vector<std::sync::Arc<T>>, b: &imbl::Vector<std::sync::Arc<T>>) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| std::sync::Arc::ptr_eq(x, y))
}

fn track(p: &Project, id: TrackId) -> &project::Track {
    p.find_track(id).expect("track").1
}

#[test]
fn new_tracks_have_their_switches_at_the_defaults() {
    let f = fixture();
    for id in [f.v1, f.v2, f.a1, f.a2, f.s1] {
        let t = track(&f.project, id);
        assert!(!t.muted && !t.solo && t.visible && !t.locked, "{id}");
        assert_eq!(t.height, TrackHeight::Medium);
        assert!(t.shows_picture() == (t.kind != TrackKind::Audio));
    }
    assert_eq!(TrackHeight::default(), TrackHeight::Medium);
}

#[test]
fn each_switch_is_an_edit_with_its_own_undo_label() {
    let f = fixture();
    let mut h = History::new(f.project.clone());
    let steps: [(
        EditLabel,
        fn(&Project, &Fixture) -> Result<Project, EditError>,
    ); 9] = [
        (EditLabel::MuteTrack, |p, f| p.set_track_muted(f.a1, true)),
        (EditLabel::SoloTrack, |p, f| p.set_track_solo(f.a2, true)),
        (EditLabel::HideTrack, |p, f| {
            p.set_track_visible(f.v2, false)
        }),
        (EditLabel::TrackHeight, |p, f| {
            p.set_track_height(f.v1, TrackHeight::Large)
        }),
        (EditLabel::LockTrack, |p, f| p.set_track_locked(f.s1, true)),
        (EditLabel::UnlockTrack, |p, f| {
            p.set_track_locked(f.s1, false)
        }),
        (EditLabel::ShowTrack, |p, f| p.set_track_visible(f.v2, true)),
        (EditLabel::UnsoloTrack, |p, f| p.set_track_solo(f.a2, false)),
        (EditLabel::UnmuteTrack, |p, f| {
            p.set_track_muted(f.a1, false)
        }),
    ];
    for (label, step) in &steps {
        let next = step(h.current(), &f).expect("switch");
        assert!(!next.content_eq(h.current()), "{label:?} changes the track");
        // Only the switched track is a new node; its items are shared.
        let before = h.current().clone();
        h.commit(*label, next);
        let after = h.current();
        let changed: Vec<TrackId> = after
            .sequence(f.seq)
            .expect("seq")
            .tracks
            .iter()
            .zip(&before.sequence(f.seq).expect("seq").tracks)
            .filter(|(a, b)| !std::sync::Arc::ptr_eq(a, b))
            .map(|(a, b)| {
                assert!(shared(&a.items, &b.items) && shared(&a.transitions, &b.transitions));
                a.id
            })
            .collect();
        assert_eq!(changed.len(), 1, "{label:?}");
    }
    // The switches undo each other; only V1's height stays changed.
    let tall = f
        .project
        .set_track_height(f.v1, TrackHeight::Large)
        .expect("height");
    assert!(h.current().content_eq(&tall), "the steps undo each other");
    let mut labels = Vec::new();
    while let Some(label) = h.undo_label() {
        labels.push(label);
        h.undo();
    }
    labels.reverse();
    assert_eq!(labels, steps.map(|(l, _)| l));
    assert!(h.current().content_eq(&f.project));
    // Halfway back: muted, soloed, hidden, tall.
    for _ in 0..4 {
        h.redo();
    }
    let p = h.current();
    assert!(track(p, f.a1).muted && track(p, f.a2).solo && !track(p, f.v2).visible);
    assert_eq!(track(p, f.v1).height, TrackHeight::Large);
    assert_eq!(
        [
            EditLabel::MuteTrack,
            EditLabel::UnmuteTrack,
            EditLabel::SoloTrack,
            EditLabel::UnsoloTrack,
            EditLabel::HideTrack,
            EditLabel::ShowTrack,
            EditLabel::LockTrack,
            EditLabel::UnlockTrack,
            EditLabel::TrackHeight,
        ]
        .map(EditLabel::i18n_key),
        [
            "edit.label.mute_track",
            "edit.label.unmute_track",
            "edit.label.solo_track",
            "edit.label.unsolo_track",
            "edit.label.hide_track",
            "edit.label.show_track",
            "edit.label.lock_track",
            "edit.label.unlock_track",
            "edit.label.track_height",
        ]
    );
}

#[test]
fn setting_a_switch_to_its_value_returns_the_same_root() {
    let f = fixture();
    let p = &f.project;
    for next in [
        p.set_track_muted(f.a1, false),
        p.set_track_solo(f.a1, false),
        p.set_track_visible(f.v1, true),
        p.set_track_locked(f.v1, false),
        p.set_track_height(f.v1, TrackHeight::Medium),
    ] {
        assert!(next.expect("no-op").content_eq(p));
    }
}

#[test]
fn each_kind_has_its_switches() {
    let f = fixture();
    let p = &f.project;
    let no = |track: TrackId, kind: TrackKind, switch: &'static str| {
        Err(EditError::NoSuchSwitch {
            track,
            kind,
            switch,
        })
    };
    assert_eq!(
        p.set_track_muted(f.v1, true),
        no(f.v1, TrackKind::Video, "mute")
    );
    assert_eq!(
        p.set_track_solo(f.s1, true),
        no(f.s1, TrackKind::Subtitle, "solo")
    );
    assert_eq!(
        p.set_track_visible(f.a1, false),
        no(f.a1, TrackKind::Audio, "visibility")
    );
    // Even setting the default is refused on a kind without the switch.
    assert_eq!(
        p.set_track_visible(f.a1, true),
        no(f.a1, TrackKind::Audio, "visibility")
    );
    let missing = TrackId(9_999);
    for result in [
        p.set_track_muted(missing, true),
        p.set_track_solo(missing, true),
        p.set_track_visible(missing, false),
        p.set_track_locked(missing, true),
        p.set_track_height(missing, TrackHeight::Small),
    ] {
        assert_eq!(result, Err(EditError::UnknownTrack(missing)));
    }
    // Every kind has a lock and a height.
    for id in [f.v1, f.a1, f.s1] {
        let locked = p.set_track_locked(id, true).expect("lock");
        assert!(track(&locked, id).locked);
        let small = p.set_track_height(id, TrackHeight::Small).expect("height");
        assert_eq!(track(&small, id).height, TrackHeight::Small);
    }
}

#[test]
fn solo_silences_the_other_audio_tracks_and_mute_wins() {
    let f = fixture();
    let seq = |p: &Project| p.sequence(f.seq).expect("seq").clone();
    let s = seq(&f.project);
    assert!(!s.any_solo());
    assert!(s.audible(f.a1) && s.audible(f.a2));
    assert!(!s.audible(f.v1), "picture tracks are never heard");
    assert!(!s.audible(TrackId(9_999)));

    let p = f.project.set_track_solo(f.a2, true).expect("solo");
    let s = seq(&p);
    assert!(s.any_solo());
    assert!(!s.audible(f.a1) && s.audible(f.a2));
    let p = p.set_track_muted(f.a2, true).expect("mute");
    assert!(!seq(&p).audible(f.a2), "mute wins over solo");
    assert!(!seq(&p).audible(f.a1), "the solo still silences the others");
    let p = p.set_track_solo(f.a1, true).expect("solo a1");
    let s = seq(&p);
    assert!(s.audible(f.a1) && !s.audible(f.a2));
    let p = p.set_track_muted(f.a1, true).expect("mute a1");
    assert!(!seq(&p).audible(f.a1));
    // Hiding a picture track is not hearing: visibility leaves audio alone.
    let p = f.project.set_track_visible(f.v1, false).expect("hide");
    assert!(seq(&p).audible(f.a1));
}

#[test]
fn a_locked_track_refuses_every_edit_to_it_and_to_what_is_on_it() {
    let f = fixture();
    let p = f.project.set_track_locked(f.v1, true).expect("lock");
    let lut = effects::lut();
    let (with_fx, fx) = f
        .project
        .add_effect(f.c1, &lut, None)
        .expect("effect before the lock");
    let fx_locked = with_fx.set_track_locked(f.v1, true).expect("lock");
    let (with_marker, marker) = f
        .project
        .add_marker(
            MarkerOwner::Clip(f.c1),
            MarkerSpec::at(frames(3), ColourLabel::Red),
        )
        .expect("marker before the lock");
    let marker_locked = with_marker.set_track_locked(f.v1, true).expect("lock");
    let transition = track(&p, f.v1).transitions[0].id;
    let spec = ClipSpec {
        media: f.media,
        range: range(200, 10),
        source_start: Time::ZERO,
    };
    let checks: Vec<(&str, Result<Project, EditError>)> = vec![
        ("insert", p.insert_clip(f.v1, spec.clone()).map(|(p, _)| p)),
        ("delete", p.remove_item(f.c2)),
        ("move within", p.move_item(f.c2, f.v1, frames(120))),
        ("move out", p.move_item(f.c2, f.v2, frames(120))),
        ("move in", p.move_item(f.top, f.v1, frames(150))),
        ("trim", p.trim_item(f.c2, range(55, 45))),
        ("slip", p.slip_clip(f.c1, frames(5))),
        ("enable", p.set_clip_enabled(f.c1, false)),
        ("opacity", p.set_clip_opacity(f.c1, 0.5)),
        ("speed", {
            p.set_clip_speed(f.c2, Rational::new(2, 1), project::SpeedKeep::Timeline)
        }),
        ("add effect", p.add_effect(f.c1, &lut, None).map(|(p, _)| p)),
        ("effect param", { fx_locked.set_effect_enabled(fx, false) }),
        ("animate", {
            p.animate_param(
                KeyTarget::Clip {
                    item: f.c1,
                    param: ClipParam::Opacity,
                },
                Time::ZERO,
            )
        }),
        ("clip marker", {
            p.add_marker(
                MarkerOwner::Clip(f.c1),
                MarkerSpec::at(frames(2), ColourLabel::Blue),
            )
            .map(|(p, _)| p)
        }),
        ("remove clip marker", marker_locked.remove_marker(marker)),
        ("add transition", {
            p.add_transition(TransitionSpec::new(Some(f.c2), None, frames(10)))
                .map(|(p, _)| p)
        }),
        (
            "transition duration",
            p.set_transition_duration(transition, frames(6)),
        ),
        ("remove transition", p.remove_transition(transition)),
        ("rename", p.rename_track(f.v1, "Picture")),
        ("hide", p.set_track_visible(f.v1, false)),
        ("remove track", p.remove_track(f.v1)),
        (
            "source",
            p.set_clip_source(f.c1, ClipSource::Media(f.media)),
        ),
        ("nest", p.nest_items(&[f.c1], "Nest").map(|(p, _, _)| p)),
    ];
    for (what, result) in checks {
        // "source" sets the source it already has: a no-op, which no lock refuses.
        if what == "source" {
            assert!(result.expect(what).content_eq(&p), "{what}");
            continue;
        }
        assert_eq!(result.err(), Some(EditError::TrackLocked(f.v1)), "{what}");
    }
    // An empty locked track keeps its kind too.
    let s = f.project.set_track_locked(f.s1, true).expect("lock s1");
    assert_eq!(
        s.set_track_kind(f.s1, TrackKind::Adjustment),
        Err(EditError::TrackLocked(f.s1))
    );

    // Unlocking and the height go through, and the other tracks are free.
    assert!(!track(&p.set_track_locked(f.v1, false).expect("unlock"), f.v1).locked);
    p.set_track_height(f.v1, TrackHeight::Small)
        .expect("height");
    p.insert_clip(f.v2, spec.clone()).expect("other track");
    p.set_clip_opacity(f.top, 0.5).expect("other clip");
    p.set_track_muted(f.a1, true).expect("other switch");
    p.add_track(f.seq, TrackKind::Video, "V3", Some(0))
        .expect("new track");
    // Edits outside tracks never meet the lock.
    p.add_marker(
        MarkerOwner::Sequence(f.seq),
        MarkerSpec::at(frames(5), ColourLabel::Green),
    )
    .expect("sequence marker");
    p.relink_media(&[Relink {
        media: f.media,
        path: "/moved/a.mov".into(),
        content_hash: "00ff".into(),
        duration: None,
    }])
    .expect("relink");
    p.rename_media(f.media, "Harbour").expect("pool edit");
    let mut settings = p.settings().clone();
    settings.colour_space = project::ColourSpace::Rec2020Pq;
    p.set_project_settings(settings).expect("settings");
}

#[test]
fn the_lock_comes_after_the_edits_own_errors() {
    let f = fixture();
    let p = f.project.set_track_locked(f.v1, true).expect("lock");
    // An overlap on the locked track is reported as the overlap.
    assert_eq!(
        p.trim_item(f.c1, range(0, 60)),
        Err(EditError::Overlap {
            track: f.v1,
            other: f.c2,
        })
    );
    assert_eq!(
        p.remove_item(ItemId(9_999)),
        Err(EditError::UnknownItem(ItemId(9_999)))
    );
    assert_eq!(
        p.rename_track(f.v1, "  "),
        Err(EditError::EmptyTrackName(f.v1))
    );
    // A switch: unknown track, then the kind's switches, then a no-op, then the lock.
    assert_eq!(
        p.set_track_muted(f.v1, true),
        Err(EditError::NoSuchSwitch {
            track: f.v1,
            kind: TrackKind::Video,
            switch: "mute",
        })
    );
    assert!(
        p.set_track_visible(f.v1, true)
            .expect("no-op")
            .content_eq(&p)
    );
    assert_eq!(
        p.set_track_visible(f.v1, false),
        Err(EditError::TrackLocked(f.v1))
    );
}

#[test]
fn moving_onto_or_breaking_apart_into_a_locked_track_is_refused() {
    let f = fixture();
    let (p, _, nested) = f.project.nest_items(&[f.top], "Top").expect("nest");
    let p = p.set_track_locked(f.v2, true).expect("lock");
    assert_eq!(p.break_apart(nested), Err(EditError::TrackLocked(f.v2)));
    assert_eq!(
        p.duplicate_compound(nested).map(|_| ()),
        Err(EditError::TrackLocked(f.v2))
    );
    let p = p.set_track_locked(f.v2, false).expect("unlock");
    p.break_apart(nested).expect("unlocked");
    // A locked audio track takes no sound clips either.
    let p = f.project.set_track_locked(f.a2, true).expect("lock a2");
    assert_eq!(
        p.move_item(f.sound, f.a2, Time::ZERO),
        Err(EditError::TrackLocked(f.a2))
    );
}

#[test]
fn deleting_a_sequence_is_not_an_edit_to_its_locked_tracks() {
    let f = fixture();
    let (p, other) = f.project.add_sequence("Other", FPS, 48_000).expect("seq");
    let (p, t) = p
        .add_track(other, TrackKind::Video, "V1", None)
        .expect("track");
    let p = p.set_track_locked(t, true).expect("lock");
    let p = p.remove_sequence(other).expect("remove");
    assert!(p.sequence(other).is_none());
}

#[test]
fn changing_a_tracks_kind_resets_the_switches_it_loses() {
    let p = Project::new("uid-kind", "Kinds");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, t) = p.add_track(seq, TrackKind::Audio, "A1", None).expect("a1");
    let p = p.set_track_muted(t, true).expect("mute");
    let p = p.set_track_solo(t, true).expect("solo");
    let p = p.set_track_height(t, TrackHeight::Large).expect("height");
    let p = p.set_track_kind(t, TrackKind::Video).expect("video");
    let v = track(&p, t);
    assert!(!v.muted && !v.solo && v.visible);
    assert_eq!(v.height, TrackHeight::Large, "the height stays");
    let p = p.set_track_visible(t, false).expect("hide");
    let p = p.set_track_kind(t, TrackKind::Audio).expect("audio");
    assert!(track(&p, t).visible);
    p.validate().expect("valid");
}

#[test]
fn switches_save_and_load() {
    let f = fixture();
    let p = f.project.set_track_muted(f.a1, true).expect("mute");
    let p = p.set_track_solo(f.a2, true).expect("solo");
    let p = p.set_track_visible(f.s1, false).expect("hide");
    let p = p.set_track_locked(f.v1, true).expect("lock");
    let p = p
        .set_track_height(f.v2, TrackHeight::Small)
        .expect("height");
    let bytes = schema::save(&p);
    let back = schema::load(&bytes).expect("load");
    assert!(back.content_eq(&p));
    assert_eq!(schema::save(&back), bytes);
    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let tracks = &json["project"]["sequences"][0]["tracks"];
    // The switches sit between the name and the items, in declaration order.
    let text = String::from_utf8(bytes.clone()).expect("UTF-8");
    let s1 = &text[text.find("\"name\": \"S1\"").expect("S1")..];
    let at: Vec<usize> = ["muted", "solo", "visible", "locked", "height", "items"]
        .iter()
        .map(|k| s1.find(&format!("\"{k}\":")).expect(k))
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    assert_eq!(tracks[0]["visible"], Value::Bool(false));
    assert_eq!(tracks[1]["height"], Value::from("small"));
    assert_eq!(tracks[2]["locked"], Value::Bool(true));
    assert_eq!(tracks[3]["muted"], Value::Bool(true));
    assert_eq!(tracks[4]["solo"], Value::Bool(true));
}

#[test]
fn files_with_switches_a_track_cannot_have_are_refused() {
    let f = fixture();
    let bytes = schema::save(&f.project);
    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let cases = [
        (2, "muted", Value::Bool(true), TrackKind::Video, "mute"),
        (0, "solo", Value::Bool(true), TrackKind::Subtitle, "solo"),
        (
            3,
            "visible",
            Value::Bool(false),
            TrackKind::Audio,
            "visibility",
        ),
    ];
    for (index, key, value, kind, switch) in cases {
        let mut broken = json.clone();
        let t = &mut broken["project"]["sequences"][0]["tracks"][index];
        let id = TrackId(t["id"].as_u64().expect("id"));
        t[key] = value;
        let err = schema::load(&serde_json::to_vec(&broken).expect("JSON")).expect_err(key);
        match err {
            LoadError::Invalid(ValidationError::Track { track, error }) => {
                assert_eq!(track, id);
                assert_eq!(
                    error,
                    EditError::NoSuchSwitch {
                        track: id,
                        kind,
                        switch,
                    }
                );
            }
            other => panic!("{key}: {other}"),
        }
    }
    let mut broken = json.clone();
    broken["project"]["sequences"][0]["tracks"][0]["height"] = Value::from("huge");
    assert!(matches!(
        schema::load(&serde_json::to_vec(&broken).expect("JSON")),
        Err(LoadError::Model { .. })
    ));
    let mut broken = json;
    let t = broken["project"]["sequences"][0]["tracks"][0]
        .as_object_mut()
        .expect("track");
    t.remove("locked");
    assert!(matches!(
        schema::load(&serde_json::to_vec(&broken).expect("JSON")),
        Err(LoadError::Model { .. })
    ));
}
