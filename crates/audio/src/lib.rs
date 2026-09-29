//! Real-time audio for Glass: device I/O through `cpal`, the audio callback, the mixer
//! graph, audio effects, meters, the master clock and voice-over recording. Receives f32
//! blocks through ring buffers and depends on no other Glass crate.
