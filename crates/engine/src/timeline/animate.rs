//! Animated looks: a clip whose picture properties or effect parameters have keyframe
//! curves (Decision 63) is evaluated per timeline frame, at the source time the frame
//! shows (`Clip::source_time_at`), so a key stays on its media frame through trims,
//! moves and speed changes. A clip without curves on anything it draws keeps one
//! constant [`ClipLook`] for every frame.

use std::path::PathBuf;

use project::params::effects;
use project::{Clip, ClipParam, Effect, Rational, Time};
use render::lut::LutSpace;

use super::plan::{ClipLook, EffectLook};

/// How a clip's look moves: the clip (its curves and constants) and where its item
/// starts on the timeline.
#[derive(Clone, Debug)]
pub(super) struct Animation {
    clip: Clip,
    item_start: Time,
    rate: Rational,
}

impl Animation {
    /// `None` when nothing the picture shows is animated: no curve on a clip property
    /// but the audio gain, and none on an enabled effect this build renders.
    pub(super) fn new(clip: &Clip, item_start: Time, rate: Rational) -> Option<Self> {
        let clip_animated = clip.keyframes.keys().any(|p| *p != ClipParam::GainDb);
        let effect_animated = clip
            .effects
            .iter()
            .any(|e| e.enabled && !e.keyframes.is_empty() && effect_look(e, None).is_some());
        (clip_animated || effect_animated).then(|| Self {
            clip: clip.clone(),
            item_start,
            rate,
        })
    }

    /// The source time timeline frame `t` shows (a handle outside the item's range).
    pub(super) fn source_time(&self, t: i64) -> Option<Time> {
        let time = Time::from_frames(t, self.rate).ok()?;
        self.clip.source_time_at(self.item_start, time).ok()
    }

    /// The look at timeline frame `t`; `base` (the constant look) when the frame's source
    /// time cannot be computed.
    pub(super) fn look_at(&self, base: &ClipLook, t: i64) -> ClipLook {
        match self.source_time(t) {
            Some(at) => self.look_at_source(base, at),
            None => base.clone(),
        }
    }

    /// The look at source time `at`.
    pub(super) fn look_at_source(&self, base: &ClipLook, at: Time) -> ClipLook {
        let clip = &self.clip;
        ClipLook {
            item: base.item,
            transform: clip.transform_at(at),
            crop: clip.crop_at(at),
            opacity: clip.opacity_at(at),
            blend: base.blend,
            effects: clip
                .effects
                .iter()
                .filter(|e| e.enabled)
                .filter_map(|e| effect_look(e, Some(at)))
                .collect(),
        }
    }
}

/// The renderer for an effect of the stack, with its parameters at source time `at`
/// (its constants for `None`), or `None` when this build has none (or a LUT names no
/// file).
pub(super) fn effect_look(effect: &Effect, at: Option<Time>) -> Option<EffectLook> {
    let params = &effect.params;
    match effect.schema.as_str() {
        "lut" => {
            let schema = effects::lut();
            if params.boolean(&schema, "enabled") == Some(false) {
                return None;
            }
            let file = params.text(&schema, "file").filter(|f| !f.is_empty())?;
            let space = params
                .choice(&schema, "input")
                .and_then(|id| LutSpace::from_id(&id))
                .unwrap_or_default();
            let intensity = match at {
                Some(at) => effect
                    .param_at(&schema, "intensity", at)
                    .and_then(|v| v.as_f64()),
                None => params.float(&schema, "intensity"),
            };
            Some(EffectLook::Lut {
                file: PathBuf::from(file),
                space,
                intensity: intensity.unwrap_or(1.0) as f32,
            })
        }
        _ => None,
    }
}
