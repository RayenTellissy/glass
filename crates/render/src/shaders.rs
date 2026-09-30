//! The WGSL sources of the compositor's effect shaders, from `crates/render/shaders/`.
//! Each effect shader is compiled with `common.wgsl` prepended (the full-screen vertex
//! shader and shared helpers).

use std::borrow::Cow;

/// Every effect shader by name, embedded at build time.
pub const EMBEDDED: &[(&str, &str)] = &[
    ("common", include_str!("../shaders/common.wgsl")),
    ("composite", include_str!("../shaders/composite.wgsl")),
    ("gamut", include_str!("../shaders/gamut.wgsl")),
];

/// The embedded source of one shader file, without `common.wgsl`.
pub fn embedded(name: &str) -> Option<&'static str> {
    EMBEDDED.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// `common.wgsl` followed by the named shader.
pub fn compose(common: &str, shader: &str) -> Cow<'static, str> {
    Cow::Owned(format!("{common}\n{shader}"))
}
