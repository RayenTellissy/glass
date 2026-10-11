//! The media pool embedded in the project (schema v9): import, remove, rename, labels,
//! tags and bins as project edits with undo labels and ids from the node counter;
//! removing media a clip uses is refused; relinking keeps the pool's fields; what is
//! saved; and validation of pools in files.

use project::diff::{Diff, flatten, rebuild};
use project::history::{EditLabel, History};
use project::media_pool::{Facts, MediaItem, MediaKind, ROOT};
use project::schema::{self, LoadError};
use project::validate::ValidationError;
use project::{
    BinId, ClipSpec, ColourLabel, EditError, MediaId, NodeId, Project, Rational, Relink,
    SequenceId, Time, TimeRange, TrackId, TrackKind,
};
use serde_json::Value;

const FPS: Rational = Rational::new(25, 1);

fn frames(n: i64) -> Time {
    Time::from_frames(n, FPS).expect("frames")
}

fn item(name: &str, path: &str) -> MediaItem {
    let mut item = MediaItem::new(name, MediaKind::Video, path);
    item.content_hash = format!("hash-{name}");
    item.size_bytes = 1_000;
    item.facts = Facts {
        duration_s: Some(12.5),
        width: 3840,
        height: 2160,
        frame_rate: Some((24000, 1001)),
        video_codec: Some("prores".into()),
        ..Facts::default()
    };
    item.metadata.camera_model = Some("ILME-FX6V".into());
    item
}

/// Every sequence is the very same node in both roots.
fn same_timeline(a: &Project, b: &Project) -> bool {
    a.sequences().len() == b.sequences().len()
        && a.sequences()
            .iter()
            .zip(b.sequences())
            .all(|(x, y)| std::sync::Arc::ptr_eq(x, y))
}

struct Fixture {
    project: Project,
    seq: SequenceId,
    v1: TrackId,
    /// Used by a clip on V1.
    used: MediaId,
    /// In no clip.
    spare: MediaId,
}

fn fixture() -> Fixture {
    let p = Project::new("uid-pool", "Harbour film");
    let (p, ids) = p
        .import_media(
            ROOT,
            [
                item("Harbour", "/card/harbour.mov"),
                item("Gulls", "/card/gulls.mov"),
            ],
        )
        .expect("import");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let (p, v1) = p.add_track(seq, TrackKind::Video, "V1", None).expect("v1");
    let (p, _) = p
        .insert_clip(
            v1,
            ClipSpec {
                media: ids[0],
                range: TimeRange::new(Time::ZERO, frames(25)),
                source_start: Time::ZERO,
            },
        )
        .expect("clip");
    Fixture {
        project: p,
        seq,
        v1,
        used: ids[0],
        spare: ids[1],
    }
}

#[test]
fn a_new_project_has_a_root_bin_named_after_it() {
    let p = Project::new("uid", "Harbour film");
    let pool = p.media();
    assert!(pool.is_empty());
    assert_eq!(pool.root(), ROOT);
    assert_eq!(ROOT, BinId(0));
    let root = pool.bin(ROOT).expect("root");
    assert_eq!((root.name.as_str(), root.parent), ("Harbour film", None));
    assert_eq!(pool.bin_count(), 1);
    p.validate().expect("valid");
}

#[test]
fn imports_take_ids_from_the_node_counter() {
    let p = Project::new("uid", "Film");
    let (p, bin) = p.add_bin(ROOT, " Day 1 ").expect("bin");
    assert_eq!(bin, BinId(1));
    assert_eq!(p.media().bin(bin).expect("bin").name, "Day 1");
    let (p, seq) = p.add_sequence("Main", FPS, 48_000).expect("seq");
    let mut unnamed = item("", "/card/A014_C003.MXF");
    unnamed.name = "  ".into();
    unnamed.id = MediaId(77);
    unnamed.bin = BinId(55);
    let (p, ids) = p
        .import_media(bin, [item("  Harbour ", "/card/h.mov"), unnamed])
        .expect("import");
    // After the bin (1) and the sequence (2), in order.
    assert_eq!(ids, [MediaId(3), MediaId(4)]);
    assert_eq!(p.next_id(), 5);
    assert_ne!(seq.0, ids[0].0);
    let a = p.media().get(ids[0]).expect("a");
    let b = p.media().get(ids[1]).expect("b");
    assert_eq!((a.id, a.bin, a.name.as_str()), (ids[0], bin, "Harbour"));
    assert_eq!((b.id, b.bin, b.name.as_str()), (ids[1], bin, "A014_C003"));
    assert_eq!(a.facts.width, 3840, "the import facts are kept");
    p.validate().expect("valid");

    assert_eq!(
        p.import_media(BinId(999), [item("x", "/x.mov")]).map(|_| ()),
        Err(EditError::UnknownBin(BinId(999)))
    );
    let (same, none) = p.import_media(bin, []).expect("nothing");
    assert!(same.content_eq(&p) && none.is_empty());
}

#[test]
fn bare_references_are_named_and_kinded_from_their_paths() {
    let p = Project::new("uid", "Film");
    let (p, a) = p.add_media("aa", "/footage/gulls.WAV").expect("a");
    let (p, b) = p.add_media("", "C:\\stills\\pier.jpg").expect("b");
    let (p, c) = p.add_media("cc", "/footage/A014_C003.mxf").expect("c");
    let get = |id| MediaItem::clone(p.media().get(id).expect("item"));
    assert_eq!((get(a).name, get(a).kind), ("gulls".into(), MediaKind::Audio));
    assert_eq!((get(b).name, get(b).kind), ("pier".into(), MediaKind::Still));
    assert_eq!((get(c).name, get(c).kind), ("A014_C003".into(), MediaKind::Video));
    assert_eq!(get(a).content_hash, "aa");
    assert_eq!(get(b).content_hash, "");
    assert_eq!(get(c).bin, ROOT);
    assert_eq!(get(c).facts, Facts::default());
}

#[test]
fn media_a_clip_uses_cannot_be_removed() {
    let f = fixture();
    let p = &f.project;
    let clip = p.sequence(f.seq).expect("seq").tracks[0].items[0].id;
    assert_eq!(p.media_user(f.used), Some(clip));
    assert_eq!(p.media_user(f.spare), None);
    assert_eq!(
        p.remove_media(&[f.spare, f.used]),
        Err(EditError::MediaInUse {
            media: f.used,
            item: clip,
        })
    );
    let missing = MediaId(9_999);
    assert_eq!(
        p.remove_media(&[f.used, missing]),
        Err(EditError::UnknownMedia(missing)),
        "unknown ids come before ids in use"
    );
    let removed = p.remove_media(&[f.spare, f.spare]).expect("remove");
    assert!(removed.media().get(f.spare).is_none());
    assert!(removed.media().get(f.used).is_some());
    assert!(
        same_timeline(&removed, p),
        "the timeline is untouched"
    );
    removed.validate().expect("valid");
    assert!(p.remove_media(&[]).expect("nothing").content_eq(p));
    // Once the clip is gone the media can go too.
    let without_clip = p.remove_item(clip).expect("delete clip");
    without_clip.remove_media(&[f.used]).expect("unused now");
    // Clips inside compounds count.
    let (nested, _, _) = p.nest_items(&[clip], "Compound").expect("nest");
    assert!(matches!(
        nested.remove_media(&[f.used]),
        Err(EditError::MediaInUse { media, .. }) if media == f.used
    ));
}

#[test]
fn pool_edits_are_undo_steps_with_their_labels() {
    let f = fixture();
    let mut h = History::new(f.project.clone());
    let step = |h: &mut History, label: EditLabel, p: Result<Project, EditError>| {
        h.commit(label, p.expect("edit"));
    };
    let (p, bin) = h.current().add_bin(ROOT, "Selects").expect("bin");
    h.commit(EditLabel::NewBin, p);
    let (p, inner) = h.current().add_bin(bin, "Day 2").expect("inner");
    h.commit(EditLabel::NewBin, p);
    let p = h.current().rename_media(f.spare, " Gulls take off ");
    step(&mut h, EditLabel::RenameMedia, p);
    let p = h
        .current()
        .set_media_label(&[f.used, f.spare], Some(ColourLabel::Amber));
    step(&mut h, EditLabel::LabelMedia, p);
    let p = h.current().add_media_tag(&[f.used], "Golden hour");
    step(&mut h, EditLabel::TagMedia, p);
    let p = h.current().add_media_tag(&[f.spare], " golden HOUR ");
    step(&mut h, EditLabel::TagMedia, p);
    let p = h.current().remove_media_tag(&[f.used], "GOLDEN hour");
    step(&mut h, EditLabel::UntagMedia, p);
    let p = h.current().move_media(&[f.spare], inner);
    step(&mut h, EditLabel::MoveMedia, p);
    let p = h.current().rename_bin(inner, "Day 3");
    step(&mut h, EditLabel::RenameBin, p);
    let p = h.current().move_bin(inner, ROOT);
    step(&mut h, EditLabel::MoveBin, p);
    let p = h.current().set_label_name(ColourLabel::Amber, " Selects ");
    step(&mut h, EditLabel::RenameLabel, p);
    let p = h.current().remove_bin(inner);
    step(&mut h, EditLabel::RemoveBin, p);
    let p = h.current().remove_media(&[f.spare]);
    step(&mut h, EditLabel::RemoveMedia, p);

    let p = h.current();
    p.validate().expect("valid");
    assert!(
        same_timeline(p, &f.project),
        "pool edits never touch the timeline"
    );
    let used = p.media().get(f.used).expect("used");
    assert_eq!(used.label, Some(ColourLabel::Amber));
    assert!(used.tags.is_empty());
    assert_eq!(p.media().label_name(ColourLabel::Amber), Some("Selects"));
    assert!(p.media().bin(inner).is_none());

    // Undo goes back through every step, with its label.
    let mut labels = Vec::new();
    while let Some(label) = h.undo_label() {
        labels.push(label);
        if labels.len() == 6 {
            // Right after the move into the inner bin: renamed, tagged with the first
            // spelling.
            let spare = h.current().media().get(f.spare).expect("spare").clone();
            assert_eq!(spare.name, "Gulls take off");
            assert_eq!(spare.bin, inner);
            assert_eq!(spare.tags, ["Golden hour"], "the first spelling stays");
        }
        h.undo();
    }
    assert_eq!(
        labels,
        [
            EditLabel::RemoveMedia,
            EditLabel::RemoveBin,
            EditLabel::RenameLabel,
            EditLabel::MoveBin,
            EditLabel::RenameBin,
            EditLabel::MoveMedia,
            EditLabel::UntagMedia,
            EditLabel::TagMedia,
            EditLabel::TagMedia,
            EditLabel::LabelMedia,
            EditLabel::RenameMedia,
            EditLabel::NewBin,
            EditLabel::NewBin,
        ]
    );
    assert!(h.current().content_eq(&f.project));
}

#[test]
fn pool_edits_check_values_then_ids_then_what_they_would_break() {
    let f = fixture();
    let p = &f.project;
    let missing = MediaId(9_999);
    assert_eq!(
        p.rename_media(missing, " "),
        Err(EditError::EmptyMediaName(missing))
    );
    assert_eq!(
        p.rename_media(missing, "x"),
        Err(EditError::UnknownMedia(missing))
    );
    assert_eq!(p.add_media_tag(&[missing], " "), Err(EditError::EmptyTag));
    assert_eq!(p.remove_media_tag(&[missing], ""), Err(EditError::EmptyTag));
    assert_eq!(
        p.add_media_tag(&[f.used, missing], "x"),
        Err(EditError::UnknownMedia(missing))
    );
    assert_eq!(
        p.set_media_label(&[missing], None),
        Err(EditError::UnknownMedia(missing))
    );
    assert_eq!(
        p.move_media(&[missing], BinId(888)),
        Err(EditError::UnknownMedia(missing))
    );
    assert_eq!(
        p.move_media(&[f.used], BinId(888)),
        Err(EditError::UnknownBin(BinId(888)))
    );
    assert_eq!(
        p.add_bin(BinId(888), " ").map(|_| ()),
        Err(EditError::EmptyBinName)
    );
    assert_eq!(
        p.add_bin(BinId(888), "x").map(|_| ()),
        Err(EditError::UnknownBin(BinId(888)))
    );
    assert_eq!(p.rename_bin(ROOT, ""), Err(EditError::EmptyBinName));
    assert_eq!(p.rename_bin(ROOT, "Project"), Err(EditError::RootBin));
    assert_eq!(
        p.rename_bin(BinId(888), "x"),
        Err(EditError::UnknownBin(BinId(888)))
    );
    assert_eq!(p.move_bin(ROOT, ROOT), Err(EditError::RootBin));
    assert_eq!(p.remove_bin(ROOT), Err(EditError::RootBin));
    assert_eq!(
        p.remove_bin(BinId(888)),
        Err(EditError::UnknownBin(BinId(888)))
    );
    let (p, outer) = p.add_bin(ROOT, "Outer").expect("outer");
    let (p, inner) = p.add_bin(outer, "Inner").expect("inner");
    assert_eq!(p.move_bin(outer, inner), Err(EditError::BinCycle));
    assert_eq!(p.move_bin(outer, outer), Err(EditError::BinCycle));
    assert_eq!(
        p.move_bin(inner, BinId(888)),
        Err(EditError::UnknownBin(BinId(888)))
    );
}

#[test]
fn edits_that_change_nothing_return_the_same_root() {
    let f = fixture();
    let p = f.project.add_media_tag(&[f.used], "Wide").expect("tag");
    let p = p
        .set_media_label(&[f.used], Some(ColourLabel::Blue))
        .expect("label");
    let (p, bin) = p.add_bin(ROOT, "Selects").expect("bin");
    let name = p.media().get(f.used).expect("used").name.clone();
    for next in [
        p.rename_media(f.used, &format!(" {name} ")),
        p.set_media_label(&[f.used], Some(ColourLabel::Blue)),
        p.set_media_label(&[], None),
        p.add_media_tag(&[f.used], "WIDE"),
        p.remove_media_tag(&[f.spare], "Wide"),
        p.move_media(&[f.used], ROOT),
        p.rename_bin(bin, "Selects"),
        p.move_bin(bin, ROOT),
        p.set_label_name(ColourLabel::Green, " "),
    ] {
        assert!(next.expect("no-op").content_eq(&p));
    }
}

#[test]
fn removing_a_bin_keeps_its_media_and_bins() {
    let f = fixture();
    let (p, a) = f.project.add_bin(ROOT, "A").expect("a");
    let (p, b) = p.add_bin(a, "B").expect("b");
    let p = p.move_media(&[f.used], b).expect("move");
    let p = p.move_media(&[f.spare], a).expect("move");
    let p = p.remove_bin(a).expect("remove");
    assert_eq!(p.media().bin(b).expect("b").parent, Some(ROOT));
    assert_eq!(p.media().get(f.spare).expect("spare").bin, ROOT);
    assert_eq!(p.media().get(f.used).expect("used").bin, b);
    assert_eq!(p.media().len(), 2);
    p.validate().expect("valid");
}

#[test]
fn relinking_points_the_item_at_its_new_file_and_keeps_the_rest() {
    let f = fixture();
    let p = f
        .project
        .set_media_label(&[f.used], Some(ColourLabel::Red))
        .expect("label");
    let before = project::MediaItem::clone(p.media().get(f.used).expect("used"));
    let p = p
        .relink_media(&[Relink {
            media: f.used,
            path: "/Volumes/Backup/harbour.mov".into(),
            content_hash: "new-hash".into(),
            duration: None,
        }])
        .expect("relink");
    let after = p.media().get(f.used).expect("used");
    assert_eq!(after.path, "/Volumes/Backup/harbour.mov");
    assert_eq!(after.content_hash, "new-hash");
    let mut expected = before;
    expected.path.clone_from(&after.path);
    expected.content_hash.clone_from(&after.content_hash);
    assert_eq!(**after, expected);
}

#[test]
fn the_pool_saves_loads_and_diffs() {
    let f = fixture();
    let (p, bin) = f.project.add_bin(ROOT, "Selects").expect("bin");
    let p = p.move_media(&[f.spare], bin).expect("move");
    let p = p.add_media_tag(&[f.spare], "Birds").expect("tag");
    let p = p
        .set_label_name(ColourLabel::Violet, "Music")
        .expect("label name");
    let p = p
        .set_media_label(&[f.spare], Some(ColourLabel::Violet))
        .expect("label");
    let bytes = schema::save(&p);
    let back = schema::load(&bytes).expect("load");
    assert!(back.content_eq(&p));
    assert_eq!(schema::save(&back), bytes);

    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let media = &json["project"]["media"];
    let keys: Vec<&str> = media
        .as_object()
        .expect("media")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["bins", "items", "label_names"]);
    assert_eq!(media["bins"][0]["id"], Value::from(0));
    assert_eq!(media["bins"][0]["name"], Value::from("Harbour film"));
    assert_eq!(media["label_names"]["violet"], Value::from("Music"));
    let spare = &media["items"][1];
    assert_eq!(spare["tags"], serde_json::json!(["Birds"]));
    assert_eq!(spare["bin"], Value::from(bin.0));
    assert!(spare.get("status").is_none(), "availability is never saved");

    // Autosave diffs carry pool edits: an item record, and the bins in the root record.
    let renamed = back.rename_media(f.used, "Harbour wide").expect("rename");
    let (old, new) = (flatten(&back), flatten(&renamed));
    let diff = Diff::between(&old, &new);
    assert_eq!(diff.upserts.len(), 1);
    assert_eq!(diff.upserts[0].id(), NodeId(f.used.0));
    let mut applied = old.clone();
    diff.apply(&mut applied);
    assert!(rebuild(&applied).expect("rebuild").content_eq(&renamed));
    let (with_bin, _) = renamed.add_bin(bin, "Inner").expect("bin");
    let diff = Diff::between(&new, &flatten(&with_bin));
    assert_eq!(diff.upserts.len(), 1, "bins travel in the root record");
    assert_eq!(diff.upserts[0].id(), project::diff::ROOT_ID);
}

#[test]
fn locks_do_not_reach_the_pool() {
    let f = fixture();
    let p = f.project.set_track_locked(f.v1, true).expect("lock");
    p.rename_media(f.used, "Harbour wide").expect("rename");
    p.set_media_label(&[f.used], Some(ColourLabel::Red))
        .expect("label");
    assert!(matches!(
        p.remove_media(&[f.used]),
        Err(EditError::MediaInUse { .. })
    ));
}

#[test]
fn files_with_broken_pools_are_refused() {
    let f = fixture();
    let json: Value = serde_json::from_slice(&schema::save(&f.project)).expect("JSON");
    let load = |v: &Value| schema::load(&serde_json::to_vec(v).expect("JSON"));
    let structure = |v: &Value, what: &str| match load(v) {
        Err(LoadError::Invalid(ValidationError::Structure(message))) => {
            assert!(message.contains(what), "{message}")
        }
        other => panic!("{what}: {other:?}"),
    };

    let mut v = json.clone();
    v["project"]["media"]["bins"] = serde_json::json!([]);
    structure(&v, "no root bin");
    let mut v = json.clone();
    v["project"]["media"]["items"][0]["bin"] = Value::from(42);
    structure(&v, "missing bin");
    let mut v = json.clone();
    v["project"]["media"]["bins"]
        .as_array_mut()
        .expect("bins")
        .push(serde_json::json!({ "id": 7, "name": "Lost", "parent": 99 }));
    structure(&v, "no parent bin");

    // A bin id another node uses, and one the counter never handed out.
    let seq = json["project"]["sequences"][0]["id"].as_u64().expect("id");
    let mut v = json.clone();
    v["project"]["media"]["bins"]
        .as_array_mut()
        .expect("bins")
        .push(serde_json::json!({ "id": seq, "name": "Twin", "parent": 0 }));
    assert!(matches!(
        load(&v),
        Err(LoadError::Invalid(ValidationError::DuplicateId(NodeId(id)))) if id == seq
    ));
    let mut v = json.clone();
    v["project"]["media"]["bins"]
        .as_array_mut()
        .expect("bins")
        .push(serde_json::json!({ "id": 500, "name": "Future", "parent": 0 }));
    assert!(matches!(
        load(&v),
        Err(LoadError::Invalid(ValidationError::Unallocated { .. }))
    ));
    // An item without one of its fields does not match the schema.
    let mut v = json;
    v["project"]["media"]["items"][0]
        .as_object_mut()
        .expect("item")
        .remove("tags");
    assert!(matches!(load(&v), Err(LoadError::Model { .. })));
}
