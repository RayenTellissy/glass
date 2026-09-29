//! Media I/O for Glass: a safe wrapper over FFmpeg for demux, decode, encode and probe,
//! hardware decode sessions, VFR maps, GOP indexes, stills, audio decode and resampling,
//! analysis algorithms and proxy generation. Produces platform frame handles or CPU
//! planes, never GPU objects. `unsafe` is allowed only in `media::ffi`.
