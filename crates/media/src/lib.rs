//! Media I/O for Glass: a safe wrapper over FFmpeg for demux, decode, encode and probe,
//! hardware decode sessions, VFR maps, GOP indexes, stills, audio decode and resampling,
//! analysis algorithms and proxy generation. Produces platform frame handles or CPU
//! planes, never GPU objects. `unsafe` is allowed only in `media::ffi`.

/// Names of the optional codec features compiled into this build.
pub fn enabled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "codec-av1") {
        features.push("codec-av1");
    }
    if cfg!(feature = "codec-prores-sw") {
        features.push("codec-prores-sw");
    }
    features
}
