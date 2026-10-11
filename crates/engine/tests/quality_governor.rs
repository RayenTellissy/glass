//! The adaptive quality governor on synthetic timelines of frame costs: it steps down on
//! misses and thin headroom, holds still under steady borderline load, backs off from a
//! level it cannot keep, restores full resolution when idle, and drops frames only at
//! the lowest level.

use std::time::Duration;

use engine::quality::{
    Admit, FrameTiming, GovernorConfig, LevelChange, QualityGovernor, QualityLevel,
};
use render::transform::Resample;

/// 60 fps.
const BUDGET: Duration = Duration::from_micros(16_667);

fn of_budget(fraction: f64) -> Duration {
    BUDGET.mul_f64(fraction)
}

/// What happened to one frame of a synthetic run.
#[derive(Clone, Copy, Debug)]
struct Step {
    frame: usize,
    admit: Admit,
    change: Option<LevelChange>,
}

/// Plays `frames` frames, each costing `cost(level, frame)` of render time.
fn play(
    governor: &mut QualityGovernor,
    frames: usize,
    mut cost: impl FnMut(QualityLevel, usize) -> Duration,
) -> Vec<Step> {
    (0..frames)
        .map(|frame| {
            let admit = governor.admit();
            let change = match admit {
                Admit::Render(level) => governor.record(FrameTiming {
                    budget: BUDGET,
                    decode_late: Duration::ZERO,
                    render: cost(level, frame),
                }),
                Admit::Drop => None,
            };
            Step {
                frame,
                admit,
                change,
            }
        })
        .collect()
}

fn changes(steps: &[Step]) -> Vec<(usize, LevelChange)> {
    steps
        .iter()
        .filter_map(|s| s.change.map(|c| (s.frame, c)))
        .collect()
}

fn drops(steps: &[Step]) -> usize {
    steps.iter().filter(|s| s.admit == Admit::Drop).count()
}

/// Full resolution costs `full` of the budget, half and quarter a quarter and a
/// sixteenth of it (render cost follows the pixel count).
fn by_area(full: f64) -> impl FnMut(QualityLevel, usize) -> Duration {
    move |level, _| {
        let d = f64::from(level.divisor());
        of_budget(full / (d * d))
    }
}

#[test]
fn levels_scale_the_canvas_and_cap_the_filter_below_full() {
    assert_eq!(QualityLevel::Full.preview_size((1920, 1080)), (1920, 1080));
    assert_eq!(QualityLevel::Half.preview_size((1920, 1080)), (960, 540));
    assert_eq!(QualityLevel::Quarter.preview_size((1920, 1080)), (480, 270));
    assert_eq!(QualityLevel::Quarter.preview_size((1001, 3)), (251, 1));
    assert_eq!(QualityLevel::Full.max_resample(), Resample::Bicubic);
    assert_eq!(QualityLevel::Half.max_resample(), Resample::Bilinear);
    assert_eq!(QualityLevel::Quarter.max_resample(), Resample::Bilinear);
    let steps = [
        QualityLevel::Full.cache_step(),
        QualityLevel::Half.cache_step(),
        QualityLevel::Quarter.cache_step(),
    ];
    assert_eq!(steps, [0, 1, 2]);
}

#[test]
fn two_misses_step_down_at_once() {
    let mut governor = QualityGovernor::default();
    let steps = play(&mut governor, 30, by_area(1.25));
    assert_eq!(
        changes(&steps),
        [(
            1,
            LevelChange {
                from: QualityLevel::Full,
                to: QualityLevel::Half
            }
        )]
    );
    assert_eq!(governor.level(), QualityLevel::Half);
    assert_eq!(drops(&steps), 0);
    assert_eq!(governor.stats().misses, 2);
}

#[test]
fn thin_headroom_steps_down_once_the_window_is_full() {
    // 5% headroom, never a miss.
    let mut governor = QualityGovernor::default();
    let steps = play(&mut governor, 30, by_area(0.95));
    let changes = changes(&steps);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].0, 11, "the twelfth frame fills the window");
    assert_eq!(changes[0].1.to, QualityLevel::Half);
    assert_eq!(governor.stats().misses, 0);
}

#[test]
fn decode_lateness_counts_towards_the_cost() {
    let mut governor = QualityGovernor::default();
    for _ in 0..2 {
        governor.record(FrameTiming {
            budget: BUDGET,
            decode_late: of_budget(0.9),
            render: of_budget(0.2),
        });
    }
    assert_eq!(governor.level(), QualityLevel::Half);
}

#[test]
fn steady_borderline_load_inside_the_band_changes_nothing() {
    // Headroom between 12% and 33%, never under 10% on average or a miss, never 35%:
    // a minute at 60 fps holds full resolution.
    let mut governor = QualityGovernor::default();
    let jitter = [0.67, 0.88, 0.75, 0.80, 0.70, 0.86, 0.78, 0.72];
    let steps = play(&mut governor, 3600, |_, frame| {
        of_budget(jitter[frame % jitter.len()])
    });
    assert!(changes(&steps).is_empty(), "{:?}", changes(&steps));
    assert_eq!(drops(&steps), 0);
    assert_eq!(governor.level(), QualityLevel::Full);

    // The same band one level down holds too.
    let mut governor = QualityGovernor::default();
    play(&mut governor, 2, by_area(1.5));
    assert_eq!(governor.level(), QualityLevel::Half);
    let steps = play(&mut governor, 3600, |_, frame| {
        of_budget(jitter[frame % jitter.len()])
    });
    assert!(changes(&steps).is_empty(), "{:?}", changes(&steps));
}

#[test]
fn a_level_that_cannot_hold_is_probed_less_and_less_often() {
    // Full misses by 20%, half has 70% headroom: every step up to full fails, and each
    // failure doubles the wait before the next try, up to 16 s.
    let mut governor = QualityGovernor::default();
    let steps = play(&mut governor, 3600, by_area(1.2));
    let changes = changes(&steps);
    let ups: Vec<usize> = changes
        .iter()
        .filter(|(_, c)| c.to == QualityLevel::Full)
        .map(|(f, _)| *f)
        .collect();
    let gaps: Vec<usize> = ups.windows(2).map(|w| w[1] - w[0]).collect();
    eprintln!(
        "60 s at 60 fps, full at 120% of the budget: {} level changes, step ups at frames {ups:?}, gaps {gaps:?}",
        changes.len()
    );
    assert!(
        gaps.windows(2).all(|g| g[1] >= g[0]),
        "the waits never shrink: {gaps:?}"
    );
    assert!(ups.len() <= 6, "{} step ups in a minute", ups.len());
    assert!(changes.len() <= 2 * ups.len() + 1);
    assert_eq!(drops(&steps), 0);
    assert_eq!(governor.level(), QualityLevel::Half);
    // Without the back-off this load would step up about once a second.
    let mut eager = QualityGovernor::new(GovernorConfig {
        max_step_up_after: Duration::from_secs(1),
        ..GovernorConfig::default()
    });
    let eager_ups = changes_to(&play(&mut eager, 3600, by_area(1.2)), QualityLevel::Full);
    eprintln!("without the back-off: {eager_ups} step ups");
    assert!(eager_ups > 3 * ups.len());
}

fn changes_to(steps: &[Step], level: QualityLevel) -> usize {
    changes(steps).iter().filter(|(_, c)| c.to == level).count()
}

#[test]
fn sustained_headroom_steps_back_up_after_a_second() {
    let mut governor = QualityGovernor::default();
    play(&mut governor, 2, by_area(1.5));
    play(&mut governor, 2, by_area(5.0));
    assert_eq!(governor.level(), QualityLevel::Quarter);

    // The load goes away: a level up per second of calm (plus a window to fill).
    let steps = play(&mut governor, 200, by_area(0.2));
    let changes = changes(&steps);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert_eq!(changes[0].1.to, QualityLevel::Half);
    assert_eq!(changes[1].1.to, QualityLevel::Full);
    // Twelve frames fill the window, and with the twelfth 60 frames make a second.
    assert_eq!(changes[0].0, 11 + 59);
    assert_eq!(changes[1].0 - changes[0].0, 12 + 59);
}

#[test]
fn idle_restores_full_resolution_and_forgets_the_back_off() {
    let mut governor = QualityGovernor::default();
    play(&mut governor, 600, by_area(1.2));
    play(&mut governor, 4, by_area(30.0));
    assert_eq!(governor.level(), QualityLevel::Quarter);

    assert_eq!(
        governor.idle(),
        Some(LevelChange {
            from: QualityLevel::Quarter,
            to: QualityLevel::Full
        })
    );
    assert_eq!(governor.level(), QualityLevel::Full);
    assert_eq!(governor.idle(), None, "already full");
    let stats = governor.stats();
    assert_eq!(stats.restores, 1);
    assert_eq!(stats.level_changes(), stats.steps_down + stats.steps_up + 1);

    // Back at half after the restore, the wait before stepping up is one second again.
    play(&mut governor, 2, by_area(1.5));
    assert_eq!(governor.level(), QualityLevel::Half);
    let steps = play(&mut governor, 100, by_area(0.2));
    assert_eq!(changes(&steps).first().map(|(f, _)| *f), Some(11 + 59));
}

#[test]
fn frames_drop_only_at_the_lowest_level() {
    // Even a quarter of the pixels misses: the level walks down first, then frames drop.
    let mut governor = QualityGovernor::default();
    let steps = play(&mut governor, 600, |_, _| of_budget(1.5));
    let changes = changes(&steps);
    assert_eq!(changes.len(), 2, "{changes:?}");
    let quarter_at = changes[1].0;
    assert_eq!(changes[1].1.to, QualityLevel::Quarter);
    let first_drop = steps
        .iter()
        .find(|s| s.admit == Admit::Drop)
        .map(|s| s.frame)
        .expect("frames dropped");
    assert!(first_drop > quarter_at);
    assert!(steps[..first_drop].iter().all(|s| s.admit != Admit::Drop));
    let dropped = drops(&steps);
    eprintln!(
        "every level at 150% of the budget: half at frame {}, quarter at frame {quarter_at}, first drop at frame {first_drop}, {dropped} of 600 dropped",
        changes[0].0
    );
    // Dropping keeps the frames within a frame interval of the clock: a third of them go.
    assert!((190..=210).contains(&dropped), "{dropped} dropped");
    assert!(governor.behind() < BUDGET * 2);
    assert_eq!(governor.stats().drops, dropped as u64);

    // Heavy at full and half but fine at a quarter: no drop at all, though the first
    // frames fell far behind the clock.
    let mut governor = QualityGovernor::default();
    let steps = play(&mut governor, 600, by_area(12.0));
    assert_eq!(governor.level(), QualityLevel::Quarter);
    assert_eq!(drops(&steps), 0);
}
