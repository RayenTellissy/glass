# Transitions

One folder per transition, named by its id (lowercase ASCII letters, digits and `-`).
The app discovers them at startup (`render::transition::TransitionRegistry`); a folder
whose manifest or shader is broken is reported and skipped, never fatal. Debug builds
read this folder and reload an edited `shader.wgsl` on the next frame; release builds
embed it (`assets::transitions::EMBEDDED`).

## `transition.json`

```json
{
  "id": "cross-dissolve",
  "name": "transition.name.cross_dissolve",
  "category": "dissolve",
  "directions": ["left", "right"],
  "thumbnail": 0.5,
  "params": []
}
```

- `id`: the folder name. Projects store it, so it never changes once shipped.
- `name`: the i18n key of the display name in `assets/i18n/en.json`.
- `category`: the browser group; its label is `transition.category.<category>`.
- `directions`: the directions offered, from `left`, `right`, `up`, `down`, `up_left`,
  `up_right`, `down_left`, `down_right`; the first is the default. Empty when the
  transition has no direction.
- `thumbnail`: the progress (0..1) the browser's thumbnail is rendered at. Thumbnails are
  rendered on demand from the shader, never stored.
- `params`: up to 8 parameter definitions in the `project::params::ParamDef` JSON format
  (the inspector's schema; the effect id and label are the transition's `id` and
  `name`). Text parameters are not allowed.

## `shader.wgsl`

Compiled after `crates/render/shaders/common.wgsl` and
`crates/render/shaders/transition.wgsl`, which declares the uniform `tr` and the inputs A
(outgoing) and B (incoming), premultiplied linear light in the working space. The shader
defines `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` and must return exactly A
at progress 0 and exactly B at progress 1. See `transition.wgsl` for the uniform layout.
