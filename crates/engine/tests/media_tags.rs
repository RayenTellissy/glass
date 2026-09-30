//! Camera metadata read on import from the file itself: QuickTime mdta keys, MP4 `©xyz`
//! and `©day` atoms, the timecode track, and EXIF in a still. Skips without fixtures
//! (`tests/fixtures/generate.sh`).

use std::path::{Path, PathBuf};

use engine::media_mgmt::{ImportEvent, ImportOptions, ImportedClip, Importer, tags};
use project::media_pool::Gps;

const PHONE: &str = "tagged_phone_h264_320x180_25.mov";
const LOCATION: &str = "tagged_location_h264_320x180_25.mp4";
const JPEG: &str = "still_1080p.jpg";

fn fixture(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/generated")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("skipped: {name} missing; run tests/fixtures/generate.sh to create it");
        None
    }
}

fn import(paths: Vec<PathBuf>) -> Vec<ImportedClip> {
    Importer::scan_and_import(paths, ImportOptions::default(), Vec::new())
        .expect("import")
        .wait()
        .into_iter()
        .filter_map(|e| match e {
            ImportEvent::Imported(clip) => Some(*clip),
            _ => None,
        })
        .collect()
}

fn clip<'a>(clips: &'a [ImportedClip], file: &Path) -> &'a ImportedClip {
    clips
        .iter()
        .find(|c| c.source.files[0] == file)
        .unwrap_or_else(|| panic!("{} was not imported", file.display()))
}

#[test]
fn container_tags_reach_the_pool() {
    let (Some(phone), Some(location)) = (fixture(PHONE), fixture(LOCATION)) else {
        return;
    };
    let from_probe = tags::from_probe(&media::probe(&phone).expect("probe"));
    let clips = import(vec![phone.clone(), location.clone()]);

    let m = &clip(&clips, &phone).metadata;
    assert_eq!(from_probe, *m);
    assert_eq!(m.camera_make.as_deref(), Some("Apple"));
    assert_eq!(m.camera_model.as_deref(), Some("iPhone 15 Pro"));
    assert_eq!(
        m.lens.as_deref(),
        Some("iPhone 15 Pro back camera 6.765mm f/1.78")
    );
    assert_eq!(m.timecode.as_deref(), Some("19:48:07:11"));
    assert_eq!(m.created.as_deref(), Some("2026-08-12T19:48:07+02:00"));
    assert_eq!(
        m.gps,
        Some(Gps {
            latitude: 60.3912,
            longitude: 5.3223,
            altitude_m: Some(12.0)
        })
    );

    let m = &clip(&clips, &location).metadata;
    assert_eq!(
        m.gps,
        Some(Gps {
            latitude: 35.6586,
            longitude: 139.7454,
            altitude_m: None
        })
    );
    assert_eq!(m.created.as_deref(), Some("2026-08-12T10:00:00Z"));
    assert_eq!((m.camera_make.as_ref(), m.timecode.as_ref()), (None, None));
}

/// A big-endian EXIF block: IFD0 with Make and a pointer to an Exif IFD holding
/// DateTimeOriginal.
fn exif_block() -> Vec<u8> {
    let make = b"FUJIFILM\0";
    let date = b"2026:08:12 07:15:30\0";
    let mut b = b"MM\0\x2a\0\0\0\x08".to_vec();
    // IFD0 at 8: two entries, then the next-IFD link.
    let ifd0_end = 8 + 2 + 2 * 12 + 4;
    let make_at = ifd0_end;
    let exif_at = make_at + make.len();
    let date_at = exif_at + 2 + 12 + 4;
    b.extend_from_slice(&2u16.to_be_bytes());
    b.extend_from_slice(&[0x01, 0x0F, 0, 2]);
    b.extend_from_slice(&(make.len() as u32).to_be_bytes());
    b.extend_from_slice(&(make_at as u32).to_be_bytes());
    b.extend_from_slice(&[0x87, 0x69, 0, 4, 0, 0, 0, 1]);
    b.extend_from_slice(&(exif_at as u32).to_be_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(make);
    b.extend_from_slice(&1u16.to_be_bytes());
    b.extend_from_slice(&[0x90, 0x03, 0, 2]);
    b.extend_from_slice(&(date.len() as u32).to_be_bytes());
    b.extend_from_slice(&(date_at as u32).to_be_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(date);
    b
}

#[test]
fn still_exif_reaches_the_pool() {
    let Some(jpeg) = fixture(JPEG) else { return };
    let dir = std::env::temp_dir().join(format!("glass-tags-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let plain = std::fs::read(&jpeg).unwrap();
    let block = exif_block();
    let mut file = plain[..2].to_vec();
    file.extend_from_slice(&[0xFF, 0xE1]);
    file.extend_from_slice(&((block.len() + 8) as u16).to_be_bytes());
    file.extend_from_slice(b"Exif\0\0");
    file.extend_from_slice(&block);
    file.extend_from_slice(&plain[2..]);
    let path = dir.join("DSCF0042.JPG");
    std::fs::write(&path, file).unwrap();

    let clips = import(vec![path.clone()]);
    let m = &clip(&clips, &path).metadata;
    assert_eq!(m.camera_make.as_deref(), Some("FUJIFILM"));
    assert_eq!(m.created.as_deref(), Some("2026-08-12T07:15:30"));
    std::fs::remove_dir_all(&dir).ok();
}
