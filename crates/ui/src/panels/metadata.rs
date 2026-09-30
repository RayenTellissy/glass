//! The metadata panel from `Sheet / Media pool` (page `02 Edit workspace`): the strip
//! under the media pool's list that shows the selected clip's camera, lens, reel,
//! timecode, recording date, position, colour, audio and file. A value the file does not
//! carry shows the board's empty mark (a muted mono dash). [`MetadataView`] maps a pool
//! item's [`Facts`] and [`Metadata`] to the strings drawn; nothing here reads the disk.

use egui::{Align2, Rect, Sense, Stroke, Ui, pos2, vec2};
use engine::media_mgmt::tags;
use project::media_pool::{Facts, Gps, Metadata};

use super::format;
use super::widgets::{Paint, Type, size};
use crate::t;
use crate::theme::Role;

/// Top border, 28 pt header, 12 pt padding around three 14 pt rows 6 pt apart.
pub const METADATA_PANEL_HEIGHT: f32 = 1.0 + size::ROW + 12.0 + 3.0 * 14.0 + 2.0 * 6.0 + 12.0;
/// The label column of each row.
const LABEL_WIDTH: f32 = 72.0;
const COLUMN_GAP: f32 = 24.0;
const ROW_GAP: f32 = 6.0;
const PADDING: f32 = 12.0;

/// Each value as the panel shows it; `None` is drawn as the empty mark.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetadataView {
    pub clip_name: String,
    /// `Sony ILME-FX6V`.
    pub camera: Option<String>,
    /// `35 mm · f/2.8 · ISO 800`, with the lens model in place of the focal length when
    /// the file names it.
    pub lens: Option<String>,
    pub reel: Option<String>,
    /// `19:48:07:11`.
    pub timecode: Option<String>,
    /// `12 Aug 2026 · 19:48`, on the camera's clock.
    pub created: Option<String>,
    /// `60.391° N · 5.322° E`.
    pub gps: Option<String>,
    /// `Rec. 709 · 10-bit 4:2:2`.
    pub colour: Option<String>,
    /// `2 ch · 48 kHz · 24-bit`.
    pub audio: Option<String>,
    /// `A014_C003.mov · 2.1 GB`.
    pub file: Option<String>,
}

impl MetadataView {
    /// The view of one pool item: its name, the file's name and size when known, and the
    /// facts and metadata read on import.
    pub fn new(
        clip_name: &str,
        file_name: Option<&str>,
        size_bytes: Option<u64>,
        facts: &Facts,
        metadata: &Metadata,
    ) -> Self {
        Self {
            clip_name: clip_name.to_string(),
            camera: camera(metadata),
            lens: lens(metadata),
            reel: metadata.reel.clone(),
            timecode: metadata.timecode.clone(),
            created: metadata.created.as_deref().map(created),
            gps: metadata.gps.map(gps),
            colour: colour(facts),
            audio: audio(facts),
            file: match (file_name, size_bytes) {
                (Some(name), Some(bytes)) => Some(format!("{name} · {}", format::bytes(bytes))),
                (Some(name), None) => Some(name.to_string()),
                (None, _) => None,
            },
        }
    }

    /// The three columns of label and value, in the board's order.
    fn columns(&self) -> [[(String, Option<&str>, Type); 3]; 3] {
        fn v(s: &Option<String>) -> Option<&str> {
            s.as_deref()
        }
        [
            [
                (t!("metadata.label.camera"), v(&self.camera), Type::Caption),
                (t!("metadata.label.lens"), v(&self.lens), Type::Caption),
                (t!("metadata.label.reel"), v(&self.reel), Type::MonoCaption),
            ],
            [
                (
                    t!("metadata.label.timecode"),
                    v(&self.timecode),
                    Type::MonoCaption,
                ),
                (
                    t!("metadata.label.created"),
                    v(&self.created),
                    Type::MonoCaption,
                ),
                (t!("metadata.label.gps"), v(&self.gps), Type::MonoCaption),
            ],
            [
                (t!("metadata.label.colour"), v(&self.colour), Type::Caption),
                (
                    t!("metadata.label.audio"),
                    v(&self.audio),
                    Type::MonoCaption,
                ),
                (t!("metadata.label.file"), v(&self.file), Type::MonoCaption),
            ],
        ]
    }
}

/// Make and model, without the make twice (`Canon` + `Canon EOS R5`).
fn camera(m: &Metadata) -> Option<String> {
    match (m.camera_make.as_deref(), m.camera_model.as_deref()) {
        (Some(make), Some(model))
            if model
                .to_ascii_lowercase()
                .starts_with(&make.to_ascii_lowercase()) =>
        {
            Some(model.to_string())
        }
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (make, model) => make.or(model).map(str::to_string),
    }
}

/// `2.8`, `4`, `24.5`.
fn short_number(value: f32) -> String {
    let s = format!("{value:.1}");
    s.trim_end_matches(".0").to_string()
}

fn lens(m: &Metadata) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(name) = &m.lens {
        parts.push(name.clone());
    } else if let Some(mm) = m.focal_length_mm {
        parts.push(t!("metadata.lens.focal", mm = short_number(mm)));
    }
    if let Some(f) = m.f_number {
        parts.push(t!("metadata.lens.f_number", value = short_number(f)));
    }
    if let Some(iso) = m.iso {
        parts.push(t!("metadata.lens.iso", value = iso));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn month(m: u8) -> String {
    match m {
        1 => t!("metadata.month.1"),
        2 => t!("metadata.month.2"),
        3 => t!("metadata.month.3"),
        4 => t!("metadata.month.4"),
        5 => t!("metadata.month.5"),
        6 => t!("metadata.month.6"),
        7 => t!("metadata.month.7"),
        8 => t!("metadata.month.8"),
        9 => t!("metadata.month.9"),
        10 => t!("metadata.month.10"),
        11 => t!("metadata.month.11"),
        _ => t!("metadata.month.12"),
    }
}

/// The recording date on the camera's clock (its offset is not applied); a value that
/// does not parse is shown as stored.
fn created(value: &str) -> String {
    let Some(d) = tags::parse_date(value) else {
        return value.to_string();
    };
    let (day, month, year) = (d.day, month(d.month), d.year);
    match d.time {
        Some((h, m, _)) => {
            let time = format!("{h:02}:{m:02}");
            t!(
                "metadata.created.date_time",
                day = day,
                month = month,
                year = year,
                time = time
            )
        }
        None => t!(
            "metadata.created.date",
            day = day,
            month = month,
            year = year
        ),
    }
}

fn gps(g: Gps) -> String {
    let lat = format!("{:.3}", g.latitude.abs());
    let lon = format!("{:.3}", g.longitude.abs());
    let ns = if g.latitude < 0.0 {
        t!("metadata.gps.south")
    } else {
        t!("metadata.gps.north")
    };
    let ew = if g.longitude < 0.0 {
        t!("metadata.gps.west")
    } else {
        t!("metadata.gps.east")
    };
    t!("metadata.gps", lat = lat, ns = ns, lon = lon, ew = ew)
}

fn colour(f: &Facts) -> Option<String> {
    if !f.has_video() {
        return None;
    }
    let space = match (f.hdr.as_deref(), f.primaries.as_deref()) {
        (Some("pq"), _) => Some(t!("metadata.colour.pq")),
        (Some("hlg"), _) => Some(t!("metadata.colour.hlg")),
        (_, Some("bt709")) => Some(t!("metadata.colour.bt709")),
        (_, Some("bt470bg" | "smpte170m")) => Some(t!("metadata.colour.bt601")),
        (_, Some("bt2020")) => Some(t!("metadata.colour.bt2020")),
        (_, Some("smpte431")) => Some(t!("metadata.colour.dci_p3")),
        (_, Some("smpte432")) => Some(t!("metadata.colour.display_p3")),
        _ => None,
    };
    let depth = match (f.bit_depth, f.chroma.as_deref()) {
        (0, _) => None,
        (bits, Some(chroma)) => Some(t!("metadata.colour.depth", bits = bits, chroma = chroma)),
        (bits, None) => Some(t!("metadata.audio.depth", bits = bits)),
    };
    let parts: Vec<String> = space.into_iter().chain(depth).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn audio(f: &Facts) -> Option<String> {
    if !f.has_audio() {
        return None;
    }
    let mut parts = vec![t!("metadata.audio.channels", count = f.audio_channels)];
    if f.sample_rate > 0 {
        let khz = format!("{:.1}", f.sample_rate as f64 / 1000.0);
        let khz = khz.trim_end_matches(".0");
        parts.push(t!("metadata.audio.rate", value = khz));
    }
    if let Some(bits) = f.audio_bit_depth {
        parts.push(t!("metadata.audio.depth", bits = bits));
    }
    Some(parts.join(" · "))
}

/// What the user did in the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataAction {
    None,
    /// `Edit…` was clicked: open the metadata editor for the clip.
    Edit,
}

/// Draws the panel at the top of `rect` ([`METADATA_PANEL_HEIGHT`] tall, full width).
pub fn metadata_panel(ui: &mut Ui, rect: Rect, view: &MetadataView) -> MetadataAction {
    let paint = Paint::new(ui.ctx());
    let rect = Rect::from_min_size(rect.min, vec2(rect.width(), METADATA_PANEL_HEIGHT));
    let painter = ui.painter_at(rect);
    let border = Stroke::new(1.0, paint.c(Role::BorderSubtle));
    painter.hline(rect.x_range(), rect.min.y + 0.5, border);

    // Header: caps title and clip name on one baseline, `Edit…` at the right.
    let header = Rect::from_min_size(rect.min + vec2(0.0, 1.0), vec2(rect.width(), size::ROW));
    painter.hline(header.x_range(), header.max.y - 0.5, border);
    let mid = header.center().y - 0.5;
    let edit_label = t!("metadata.edit");
    let edit_w = paint.width(&painter, &edit_label, Type::Label);
    let edit_rect = Rect::from_center_size(
        pos2(header.max.x - PADDING - edit_w / 2.0, mid),
        vec2(edit_w + 8.0, size::SMALL_CONTROL),
    );
    let response = ui.interact(edit_rect, ui.id().with("metadata.edit"), Sense::click());
    paint.text(
        &painter,
        pos2(header.max.x - PADDING, mid),
        Align2::RIGHT_CENTER,
        &edit_label,
        Type::Label,
        if response.hovered() {
            Role::TextPrimary
        } else {
            Role::TextSecondary
        },
        edit_w + 1.0,
    );
    let title = paint.text(
        &painter,
        pos2(header.min.x + PADDING, mid),
        Align2::LEFT_CENTER,
        &t!("metadata.title"),
        Type::Caps,
        Role::TextMuted,
        header.width(),
    );
    paint.text(
        &painter,
        pos2(title.max.x + 8.0, mid),
        Align2::LEFT_CENTER,
        &view.clip_name,
        Type::Label,
        Role::TextPrimary,
        (edit_rect.min.x - 16.0 - title.max.x).max(1.0),
    );

    // Body: three equal columns of label and value rows.
    let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max).shrink(PADDING);
    let column_w = ((body.width() - 2.0 * COLUMN_GAP) / 3.0).max(1.0);
    let row_h = paint.line_height(Type::Caption);
    let empty = t!("metadata.empty");
    for (c, column) in view.columns().iter().enumerate() {
        let x = body.min.x + c as f32 * (column_w + COLUMN_GAP);
        for (r, (label, value, ty)) in column.iter().enumerate() {
            let y = body.min.y + r as f32 * (row_h + ROW_GAP);
            paint.text(
                &painter,
                pos2(x, y),
                Align2::LEFT_TOP,
                label,
                Type::Caption,
                Role::TextMuted,
                LABEL_WIDTH,
            );
            let value_x = x + LABEL_WIDTH + 8.0;
            let value_w = (column_w - LABEL_WIDTH - 8.0).max(1.0);
            let (text, ty, role) = match value {
                Some(v) => (*v, *ty, Role::TextPrimary),
                None => (empty.as_str(), Type::MonoCaption, Role::TextMuted),
            };
            paint.text(
                &painter,
                pos2(value_x, y),
                Align2::LEFT_TOP,
                text,
                ty,
                role,
                value_w,
            );
        }
    }

    if response.clicked() {
        MetadataAction::Edit
    } else {
        MetadataAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx6() -> (Facts, Metadata) {
        let facts = Facts {
            width: 3840,
            height: 2160,
            bit_depth: 10,
            chroma: Some("4:2:2".into()),
            primaries: Some("bt709".into()),
            audio_channels: 2,
            sample_rate: 48_000,
            audio_bit_depth: Some(24),
            ..Facts::default()
        };
        let metadata = Metadata {
            camera_make: Some("Sony".into()),
            camera_model: Some("ILME-FX6V".into()),
            focal_length_mm: Some(35.0),
            f_number: Some(2.8),
            iso: Some(800),
            timecode: Some("19:48:07:11".into()),
            reel: Some("A014".into()),
            gps: Some(Gps {
                latitude: 60.391234,
                longitude: 5.322345,
                altitude_m: None,
            }),
            created: Some("2026-08-12T19:48:07+02:00".into()),
            ..Metadata::default()
        };
        (facts, metadata)
    }

    #[test]
    fn maps_a_camera_clip_like_the_board() {
        let (facts, metadata) = fx6();
        let v = MetadataView::new(
            "Harbour at dusk",
            Some("A014_C003.mov"),
            Some(2_100_000_000),
            &facts,
            &metadata,
        );
        assert_eq!(v.camera.as_deref(), Some("Sony ILME-FX6V"));
        assert_eq!(v.lens.as_deref(), Some("35 mm · f/2.8 · ISO 800"));
        assert_eq!(v.reel.as_deref(), Some("A014"));
        assert_eq!(v.timecode.as_deref(), Some("19:48:07:11"));
        assert_eq!(v.created.as_deref(), Some("12 Aug 2026 · 19:48"));
        assert_eq!(v.gps.as_deref(), Some("60.391° N · 5.322° E"));
        assert_eq!(v.colour.as_deref(), Some("Rec. 709 · 10-bit 4:2:2"));
        assert_eq!(v.audio.as_deref(), Some("2 ch · 48 kHz · 24-bit"));
        assert_eq!(v.file.as_deref(), Some("A014_C003.mov · 2.1 GB"));
    }

    #[test]
    fn missing_values_stay_empty() {
        let v = MetadataView::new(
            "Voice memo",
            None,
            None,
            &Facts::default(),
            &Metadata::default(),
        );
        assert_eq!(
            v,
            MetadataView {
                clip_name: "Voice memo".into(),
                ..MetadataView::default()
            }
        );
    }

    #[test]
    fn other_spellings() {
        let metadata = Metadata {
            camera_make: Some("Canon".into()),
            camera_model: Some("Canon EOS R5".into()),
            lens: Some("RF35mm F1.8 MACRO IS STM".into()),
            f_number: Some(4.0),
            gps: Some(Gps {
                latitude: -33.8568,
                longitude: -70.6483,
                altitude_m: None,
            }),
            created: Some("2026-01-03".into()),
            ..Metadata::default()
        };
        let facts = Facts {
            width: 1920,
            height: 1080,
            bit_depth: 10,
            chroma: Some("4:2:0".into()),
            primaries: Some("bt2020".into()),
            hdr: Some("hlg".into()),
            audio_channels: 1,
            sample_rate: 44_100,
            ..Facts::default()
        };
        let v = MetadataView::new("Street", Some("IMG_0042.MOV"), None, &facts, &metadata);
        assert_eq!(v.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(v.lens.as_deref(), Some("RF35mm F1.8 MACRO IS STM · f/4"));
        assert_eq!(v.gps.as_deref(), Some("33.857° S · 70.648° W"));
        assert_eq!(v.created.as_deref(), Some("3 Jan 2026"));
        assert_eq!(v.colour.as_deref(), Some("Rec. 2100 HLG · 10-bit 4:2:0"));
        assert_eq!(v.audio.as_deref(), Some("1 ch · 44.1 kHz"));
        assert_eq!(v.file.as_deref(), Some("IMG_0042.MOV"));
        // Only a make, and a date that does not parse, are shown as they are.
        let odd = Metadata {
            camera_make: Some("DJI".into()),
            created: Some("sometime in August".into()),
            ..Metadata::default()
        };
        let v = MetadataView::new("Drone", None, None, &Facts::default(), &odd);
        assert_eq!(v.camera.as_deref(), Some("DJI"));
        assert_eq!(v.created.as_deref(), Some("sometime in August"));
    }
}
