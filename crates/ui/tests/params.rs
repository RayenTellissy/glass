//! Parameter sections generated from schemas: what scrubbing, typing, clicking and the
//! header switch emit, driven through a plain `egui::Context` with synthetic input, and
//! an inspector column of about 30 parameters within the frame budget.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use project::params::{ParamDef, ParamSchema, ParamValue, ParamValues, Range, Unit, builtin};
use ui::frame_time::FrameTimeMonitor;
use ui::headless::{HeadlessUi, Viewport};
use ui::params::{KeyframeInfo, KeyframeMark, ParamEvent, Section, apply};
use ui::theme::{self, ThemeKind};

/// Row centres for a section drawn at the top-left: 4 px padding, a 28 px header, then
/// 28 px rows.
fn row_y(row: usize) -> f32 {
    4.0 + 28.0 + 28.0 * row as f32 + 14.0
}

/// Lanes of a 320 px row: label 8..72, fields 80..240, keyframes 248..288, reset
/// 296..312.
const FIELDS_LEFT: f32 = 80.0;
const FIELDS_RIGHT: f32 = 240.0;
const DIAMOND_X: f32 = 248.0 + 12.0 + 8.0;
const RESET_X: f32 = 304.0;

struct Harness {
    ctx: egui::Context,
    time: f64,
}

impl Harness {
    fn new() -> Self {
        let ctx = egui::Context::default();
        theme::set(&ctx, ThemeKind::Dark);
        Self { ctx, time: 0.0 }
    }

    /// One frame with `input`, the section 320 px wide at the top-left. Returns the
    /// events and the height the section used.
    fn frame(
        &mut self,
        input: Vec<Event>,
        modifiers: Modifiers,
        section: impl FnOnce(&mut egui::Ui) -> Vec<ParamEvent>,
    ) -> (Vec<ParamEvent>, f32) {
        self.time += 1.0 / 60.0;
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 800.0))),
            time: Some(self.time),
            events: std::iter::once(Event::ModifiersChanged(modifiers))
                .chain(input)
                .collect(),
            ..Default::default()
        };
        let mut out = (Vec::new(), 0.0);
        let mut section = Some(section);
        let mut output = self.ctx.run_ui(raw, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let response = ui.allocate_ui(vec2(320.0, 800.0), |ui| {
                ui.set_width(320.0);
                (section.take().expect("drawn once"))(ui)
            });
            out = (response.inner, response.response.rect.height());
        });
        // No renderer here: the font atlas upload is dropped on purpose.
        output.textures_delta.clear();
        out
    }
}

fn pointer(pos: Pos2) -> Event {
    Event::PointerMoved(pos)
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// Clicks at `at` over two frames and returns every event emitted.
fn click(
    h: &mut Harness,
    at: Pos2,
    mut draw: impl FnMut(&mut egui::Ui) -> Vec<ParamEvent>,
) -> Vec<ParamEvent> {
    let mut all = Vec::new();
    for input in [
        vec![pointer(at)],
        vec![button(at, true)],
        vec![button(at, false)],
        vec![],
    ] {
        all.extend(h.frame(input, Modifiers::NONE, &mut draw).0);
    }
    all
}

/// Clicks a number field, selects all, types `text` and presses `end` (Return or Esc).
fn type_into(
    h: &mut Harness,
    at: Pos2,
    text: &str,
    end: Key,
    mut draw: impl FnMut(&mut egui::Ui) -> Vec<ParamEvent>,
) -> (Vec<ParamEvent>, f32) {
    let mut all = click(h, at, &mut draw);
    assert!(
        all.is_empty(),
        "a click on a number only starts typing: {all:?}"
    );
    for input in [
        vec![],
        vec![key(Key::A, Modifiers::COMMAND)],
        vec![Event::Text(text.to_owned())],
        vec![key(end, Modifiers::NONE)],
    ] {
        all.extend(h.frame(input, Modifiers::NONE, &mut draw).0);
    }
    let (events, height) = h.frame(vec![], Modifiers::NONE, &mut draw);
    all.extend(events);
    (all, height)
}

fn sets(events: &[ParamEvent]) -> Vec<(String, ParamValue, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            ParamEvent::Set { id, value, commit } => Some((id.clone(), value.clone(), *commit)),
            _ => None,
        })
        .collect()
}

#[test]
fn dragging_a_number_scrubs_and_commits_once_at_the_end() {
    let schema = builtin::transform();
    let values = schema.defaults();
    let mut h = Harness::new();
    let draw = |ui: &mut egui::Ui| Section::new(&schema, &values).show(ui);
    let y = row_y(0);
    let start = pos2(100.0, y);
    let mut events = Vec::new();
    let mut input = vec![vec![pointer(start)], vec![button(start, true)]];
    for x in [110.0, 120.0, 130.0, 140.0] {
        input.push(vec![pointer(pos2(x, y))]);
    }
    input.push(vec![button(pos2(140.0, y), false)]);
    for frame in input {
        events.extend(h.frame(frame, Modifiers::NONE, draw).0);
    }
    let sets = sets(&events);
    assert!(sets.len() >= 2, "{sets:?}");
    let (last, moving) = sets.split_last().expect("events");
    assert!(moving.iter().all(|(_, _, commit)| !commit), "{moving:?}");
    // Position is unbounded: one pixel of value per point of travel.
    assert_eq!(
        last,
        &(
            "position".to_owned(),
            ParamValue::Point([1000.0, 540.0]),
            true
        )
    );
}

#[test]
fn shift_scrubs_a_tenth_as_fast_over_the_soft_range() {
    let schema = builtin::transform();
    let values = schema.defaults();
    let mut h = Harness::new();
    let draw = |ui: &mut egui::Ui| Section::new(&schema, &values).show(ui);
    let y = row_y(1);
    let start = pos2(100.0, y);
    let mut events = Vec::new();
    let frames = [
        vec![pointer(start)],
        vec![button(start, true)],
        vec![pointer(pos2(140.0, y))],
        vec![button(pos2(140.0, y), false)],
    ];
    for frame in frames {
        events.extend(h.frame(frame, Modifiers::SHIFT, draw).0);
    }
    let (id, value, commit) = sets(&events).pop().expect("a commit");
    assert_eq!((id.as_str(), commit), ("scale", true));
    // Scale's soft range is 0..4 over 400 points: 40 points is 0.4, a tenth is 0.04.
    let ParamValue::Float(v) = value else {
        panic!("scale is a float: {value:?}");
    };
    assert!((v - 1.04).abs() < 1e-9, "{v}");
}

#[test]
fn typing_commits_maths_relative_to_the_value_in_display_units() {
    let schema = builtin::compositing();
    let values = schema.defaults();
    let draw = |ui: &mut egui::Ui| Section::new(&schema, &values).show(ui);
    // Opacity has a slider, so its field is the 64 px at the right of the lane.
    let field = pos2(FIELDS_RIGHT - 20.0, row_y(0));
    let (events, _) = type_into(&mut Harness::new(), field, "50", Key::Enter, draw);
    assert_eq!(
        sets(&events),
        vec![("opacity".to_owned(), ParamValue::Float(0.5), true)]
    );
    let (events, _) = type_into(&mut Harness::new(), field, "100-25", Key::Enter, draw);
    assert_eq!(
        sets(&events),
        vec![("opacity".to_owned(), ParamValue::Float(0.75), true)]
    );

    let transform = builtin::transform();
    let t_values = transform.defaults();
    let draw = |ui: &mut egui::Ui| Section::new(&transform, &t_values).show(ui);
    let y_field = pos2(FIELDS_RIGHT - 20.0, row_y(0));
    let (events, _) = type_into(&mut Harness::new(), y_field, "+10", Key::Enter, draw);
    assert_eq!(
        sets(&events),
        vec![(
            "position".to_owned(),
            ParamValue::Point([960.0, 550.0]),
            true
        )]
    );
    let scale = pos2(FIELDS_LEFT + 20.0, row_y(1));
    let (events, _) = type_into(&mut Harness::new(), scale, "*2", Key::Enter, draw);
    assert_eq!(
        sets(&events),
        vec![("scale".to_owned(), ParamValue::Float(2.0), true)]
    );
}

#[test]
fn out_of_range_keeps_the_value_and_shows_a_helper_and_esc_cancels() {
    let schema = builtin::compositing();
    let values = schema.defaults();
    let draw = |ui: &mut egui::Ui| Section::new(&schema, &values).show(ui);
    let field = pos2(FIELDS_RIGHT - 20.0, row_y(0));
    let mut h = Harness::new();
    let (_, plain) = h.frame(vec![], Modifiers::NONE, draw);
    let (events, with_error) = type_into(&mut h, field, "140", Key::Enter, draw);
    assert!(sets(&events).is_empty(), "{events:?}");
    assert!(
        with_error > plain,
        "the error helper adds a line: {plain} -> {with_error}"
    );

    let (events, _) = type_into(&mut Harness::new(), field, "20", Key::Escape, draw);
    assert!(sets(&events).is_empty(), "{events:?}");
    let (events, _) = type_into(&mut Harness::new(), field, "abc", Key::Enter, draw);
    assert!(sets(&events).is_empty(), "{events:?}");
}

#[test]
fn keyframe_reset_switch_and_stepper_clicks() {
    let schema = builtin::transform();
    let mut values = schema.defaults();
    values.set(&schema, "scale", ParamValue::Float(2.0));
    let keys = |id: &str| KeyframeInfo {
        mark: if id == "scale" {
            KeyframeMark::AtPlayhead
        } else {
            KeyframeMark::Off
        },
        previous: id == "scale",
        next: false,
    };
    let draw = |ui: &mut egui::Ui| Section::new(&schema, &values).keyframes(&keys).show(ui);
    let events = click(&mut Harness::new(), pos2(DIAMOND_X, row_y(1)), draw);
    assert_eq!(
        events,
        vec![ParamEvent::ToggleKeyframe { id: "scale".into() }]
    );
    let events = click(&mut Harness::new(), pos2(254.0, row_y(1)), draw);
    assert_eq!(
        events,
        vec![ParamEvent::PreviousKeyframe { id: "scale".into() }]
    );
    // No keyframe after the playhead: next does nothing.
    let events = click(&mut Harness::new(), pos2(282.0, row_y(1)), draw);
    assert!(events.is_empty(), "{events:?}");
    let events = click(&mut Harness::new(), pos2(RESET_X, row_y(1)), draw);
    assert_eq!(events, vec![ParamEvent::Reset { id: "scale".into() }]);
    let events = click(&mut Harness::new(), pos2(RESET_X, 18.0), draw);
    assert_eq!(events, vec![ParamEvent::ResetSection]);
    // Rotation is at its default: no reset button.
    let events = click(&mut Harness::new(), pos2(RESET_X, row_y(2)), draw);
    assert!(events.is_empty(), "{events:?}");

    let blur = builtin::motion_blur();
    let blur_values = blur.defaults();
    let draw = |ui: &mut egui::Ui| Section::new(&blur, &blur_values).show(ui);
    // Header switch: 24 px left of the reset slot's 8 px gap.
    let events = click(&mut Harness::new(), pos2(296.0 - 8.0 - 12.0, 18.0), draw);
    assert_eq!(
        sets(&events),
        vec![("enabled".to_owned(), ParamValue::Bool(true), true)]
    );
    // While the switch is off the rows take no input.
    let events = click(
        &mut Harness::new(),
        pos2(FIELDS_LEFT + 84.0, row_y(1)),
        draw,
    );
    assert!(events.is_empty(), "{events:?}");

    let mut on = blur.defaults();
    on.set(&blur, "enabled", ParamValue::Bool(true));
    let draw = |ui: &mut egui::Ui| Section::new(&blur, &on).show(ui);
    // Samples stepper: minus 80..104, plus 152..176.
    let events = click(
        &mut Harness::new(),
        pos2(FIELDS_LEFT + 84.0, row_y(1)),
        draw,
    );
    assert_eq!(
        sets(&events),
        vec![("samples".to_owned(), ParamValue::Int(9), true)]
    );
    let events = click(
        &mut Harness::new(),
        pos2(FIELDS_LEFT + 12.0, row_y(1)),
        draw,
    );
    assert_eq!(
        sets(&events),
        vec![("samples".to_owned(), ParamValue::Int(7), true)]
    );
}

#[test]
fn a_bypassed_section_takes_no_input() {
    let schema = builtin::transform();
    let mut values = schema.defaults();
    values.set(&schema, "scale", ParamValue::Float(2.0));
    let draw = |ui: &mut egui::Ui| Section::new(&schema, &values).enabled(false).show(ui);
    for at in [
        pos2(100.0, row_y(0)),
        pos2(DIAMOND_X, row_y(1)),
        pos2(RESET_X, row_y(1)),
        pos2(RESET_X, 18.0),
    ] {
        let events = click(&mut Harness::new(), at, draw);
        assert!(events.is_empty(), "{at:?}: {events:?}");
    }
}

#[test]
fn apply_sets_resets_and_ignores_keyframe_events() {
    let schema = builtin::compositing();
    let mut values = ParamValues::new();
    apply(
        &schema,
        &mut values,
        &[
            ParamEvent::Set {
                id: "opacity".into(),
                value: ParamValue::Float(3.0),
                commit: true,
            },
            ParamEvent::Set {
                id: "blend".into(),
                value: ParamValue::Choice("screen".into()),
                commit: true,
            },
            ParamEvent::ToggleKeyframe {
                id: "opacity".into(),
            },
        ],
    );
    assert_eq!(values.float(&schema, "opacity"), Some(1.0));
    assert_eq!(values.choice(&schema, "blend").as_deref(), Some("screen"));
    apply(
        &schema,
        &mut values,
        &[ParamEvent::Reset { id: "blend".into() }],
    );
    assert!(values.is_default(&schema, "blend"));
    values.set(&schema, "opacity", ParamValue::Float(0.2));
    apply(&schema, &mut values, &[ParamEvent::ResetSection]);
    assert!(
        schema
            .params
            .iter()
            .all(|p| values.is_default(&schema, &p.id))
    );
}

#[test]
fn builtin_labels_are_english_strings() {
    let english = assets::i18n::Locale::english();
    for schema in builtin::all() {
        let mut keys = vec![schema.label.clone()];
        for p in &schema.params {
            keys.push(p.label.clone());
            if let project::params::ParamKind::Choice { options, .. } = &p.kind {
                keys.extend(options.iter().map(|o| o.label.clone()));
            }
        }
        for key in keys {
            assert!(
                english.get(&key).is_some(),
                "{}: missing string {key}",
                schema.id
            );
        }
    }
}

/// About 30 parameters: the four built-in sections, a second transform and crop (an
/// effect's own), and an effect with one of every other kind, with values moving each frame.
fn inspector_column(ui: &mut egui::Ui, frame: usize, schemas: &[ParamSchema]) {
    egui::Panel::left("inspector")
        .exact_size(320.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let phase = frame as f64 / 60.0;
            for (i, schema) in schemas.iter().enumerate() {
                let mut values = schema.defaults();
                for p in &schema.params {
                    let v = match &p.kind {
                        project::params::ParamKind::Float { .. } => {
                            Some(ParamValue::Float(0.5 + 0.4 * phase.sin()))
                        }
                        project::params::ParamKind::Point { .. } => Some(ParamValue::Point([
                            960.0 + 100.0 * phase.cos(),
                            540.0 + 50.0 * phase.sin(),
                        ])),
                        project::params::ParamKind::Angle { .. } => {
                            Some(ParamValue::Angle(frame as f64 * 3.0))
                        }
                        _ => None,
                    };
                    if let Some(v) = v {
                        values.set(schema, &p.id, v);
                    }
                }
                let keys = |id: &str| KeyframeInfo {
                    mark: match (id.len() + frame / 30) % 3 {
                        0 => KeyframeMark::Off,
                        1 => KeyframeMark::On,
                        _ => KeyframeMark::AtPlayhead,
                    },
                    previous: true,
                    next: frame.is_multiple_of(2),
                };
                let _ = Section::new(schema, &values)
                    .keyframes(&keys)
                    .id_salt(i)
                    .show(ui);
            }
        });
}

#[test]
fn inspector_column_p99_is_within_budget() {
    let Some(mut headless) = HeadlessUi::new_or_skip("params frame budget", Viewport::default())
    else {
        return;
    };
    let every_kind = ParamSchema::new(
        "every-kind",
        "sample.effect.glow",
        vec![
            ParamDef::colour("colour", "sample.param.colour", [1.0, 0.6, 0.2, 1.0], true),
            ParamDef::text("caption", "sample.param.caption", "", false),
            ParamDef::boolean("invert", "sample.param.invert", false),
            ParamDef::choice(
                "falloff",
                "sample.param.falloff",
                &[
                    ("smooth", "sample.param.falloff_smooth"),
                    ("linear", "sample.param.falloff_linear"),
                ],
            ),
            ParamDef::int("copies", "sample.param.copies", 3, Range::new(1.0, 12.0)),
            ParamDef::angle("angle", "sample.param.angle", 0.0),
            ParamDef::float(
                "radius",
                "sample.param.radius",
                24.0,
                Range::new(0.0, 500.0),
            )
            .soft(Range::new(0.0, 100.0))
            .unit(Unit::Pixels)
            .slider(),
            ParamDef::point("centre", "sample.param.centre", [960.0, 540.0]),
        ],
    );
    let mut schemas = builtin::all();
    schemas.push(builtin::transform());
    schemas.push(builtin::crop());
    schemas.push(every_kind);
    let params: usize = schemas.iter().map(|s| s.params.len()).sum();
    assert!(params >= 28, "{params} parameters");
    let mut monitor = FrameTimeMonitor::default();
    headless
        .run(30, 240, &mut monitor, |ui, frame| {
            inspector_column(ui, frame, &schemas)
        })
        .expect("headless frames");
    let stats = monitor.stats();
    eprintln!(
        "params frame budget, {params} parameters ({}): {stats}",
        headless.adapter()
    );
    headless.assert_p99_within_budget("params frame budget", &stats);
}
