//! The Glass document model: the immutable edit tree of sequences, tracks, clips,
//! transitions, keyframes and effect parameter schemas, plus undo history, save and
//! load, and schema migrations. Pure data: no I/O threads, no GPU, and no dependencies
//! on other Glass crates.
