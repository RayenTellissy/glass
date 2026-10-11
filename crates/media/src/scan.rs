//! Reading a stream's packet timestamps without decoding: the source of VFR timestamp
//! maps and GOP indexes when the container's own index cannot give presentation times.

use serde::{Deserialize, Serialize};

use crate::demux::Demuxer;
use crate::error::Result;
use crate::ffi;

/// One packet of a stream, as the demuxer reports it. Times are in the stream time base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketEntry {
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    /// Byte offset in the file, when the demuxer knows it.
    pub pos: Option<u64>,
    pub key: bool,
    /// Zero when unknown.
    pub duration: i64,
}

/// Reads every packet of stream `index` from the start of the file, keeping only
/// timestamps, positions and flags; packets of other streams are discarded by the
/// demuxer. Leaves the demuxer at the end of the file.
pub(crate) fn scan_stream(demuxer: &mut Demuxer, index: usize) -> Result<Vec<PacketEntry>> {
    ffi::keep_only_streams(demuxer.input_mut(), &[index]);
    demuxer.rewind()?;
    let mut entries = Vec::new();
    while let Some(packet) = demuxer.read_packet()? {
        if packet.stream_index() != index {
            continue;
        }
        entries.push(PacketEntry {
            pts: packet.pts().map(|t| t.value),
            dts: packet.dts().map(|t| t.value),
            pos: packet.position(),
            key: packet.is_key(),
            duration: packet.duration().map_or(0, |d| d.value),
        });
    }
    Ok(entries)
}
