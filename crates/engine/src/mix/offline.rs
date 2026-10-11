//! The offline mix for export and checks: the clip side and the mixer graph run on the
//! caller's thread as fast as decoding allows, block by block, with the same code the live
//! mix runs, so an export sounds like playback.

use std::sync::Arc;

use audio::mixer::{Graph, MeterBank};
use project::{Project, SequenceId, TimeRange};

use super::source::{SequenceSource, SourceStats};
use super::{MIX_BLOCK, MixError, MixFormat, MixSettings, graph_spec, sample_of};

/// A sequence's mix, rendered on demand from a start frame onwards.
pub struct OfflineMix {
    source: SequenceSource,
    graph: Graph,
    format: MixFormat,
    position: i64,
    mixed: Vec<f32>,
}

impl OfflineMix {
    /// A mix of `sequence` with `settings` at `format`, whose first rendered frame is mix
    /// frame `start` (sample `start` at `format`'s rate on the sequence's timeline).
    pub fn new(
        project: &Project,
        sequence: SequenceId,
        settings: &MixSettings,
        format: MixFormat,
        start: i64,
    ) -> Result<Self, MixError> {
        let spec = graph_spec(project, sequence, settings, format, MIX_BLOCK)?;
        let graph = Graph::build(&spec)?;
        let source = SequenceSource::build(project, sequence, format, None)?;
        Ok(Self {
            source,
            graph,
            format,
            position: start,
            mixed: vec![0.0; MIX_BLOCK * format.channels()],
        })
    }

    pub fn format(&self) -> MixFormat {
        self.format
    }

    pub fn channels(&self) -> usize {
        self.format.channels()
    }

    /// The mix frame the next render starts at.
    pub fn position(&self) -> i64 {
        self.position
    }

    /// The graph's meters (published as rendering passes their periods).
    pub fn meters(&self) -> Arc<MeterBank> {
        self.graph.meters()
    }

    pub fn stats(&self) -> SourceStats {
        self.source.stats()
    }

    /// Whether any audio clip plays (has media and a supported speed) within mix frames
    /// `from..to`, whatever the mixer settings.
    pub fn plays_between(&self, from: i64, to: i64) -> bool {
        self.source.plays_between(from, to)
    }

    /// Renders the next `out.len() / channels()` frames, interleaved.
    pub fn render(&mut self, out: &mut [f32]) {
        let channels = self.channels();
        let frames = out.len() / channels;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MIX_BLOCK);
            self.source
                .render(self.position, n, self.graph.input_mut(n));
            let out = &mut out[done * channels..(done + n) * channels];
            self.graph.process(self.position, n, out);
            self.position += n as i64;
            done += n;
        }
    }

    /// Renders the next `frames` frames into one plane per channel (as
    /// `ExportSession::push_audio` takes them), resizing the planes.
    pub fn render_planar(&mut self, frames: usize, planes: &mut [Vec<f32>]) {
        let channels = self.channels();
        for plane in planes.iter_mut() {
            plane.clear();
        }
        let mut mixed = std::mem::take(&mut self.mixed);
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MIX_BLOCK);
            self.render(&mut mixed[..n * channels]);
            for (c, plane) in planes.iter_mut().enumerate().take(channels) {
                plane.extend(mixed[..n * channels].iter().skip(c).step_by(channels));
            }
            done += n;
        }
        self.mixed = mixed;
    }
}

/// The mix of `range` of `sequence` at the sequence's rate and layout with every track at
/// its defaults: interleaved f32, the sequence layout's channels per frame.
pub fn mix_range(
    project: &Project,
    sequence: SequenceId,
    range: TimeRange,
) -> Result<Vec<f32>, MixError> {
    let seq = project
        .sequence(sequence)
        .ok_or(MixError::UnknownSequence(sequence))?;
    mix_range_with(
        project,
        sequence,
        range,
        &MixSettings::default(),
        MixFormat::of(seq),
    )
}

/// The mix of `range` of `sequence` with `settings` at `format`, interleaved. The range's
/// ends are rounded to the nearest sample.
pub fn mix_range_with(
    project: &Project,
    sequence: SequenceId,
    range: TimeRange,
    settings: &MixSettings,
    format: MixFormat,
) -> Result<Vec<f32>, MixError> {
    if format.sample_rate == 0 {
        return Err(MixError::ZeroRate);
    }
    let start = sample_of(range.start, format.sample_rate)?;
    let end = sample_of(range.end()?, format.sample_rate)?;
    let mut mix = OfflineMix::new(project, sequence, settings, format, start)?;
    let mut out = vec![0.0; (end - start).max(0) as usize * format.channels()];
    mix.render(&mut out);
    Ok(out)
}
