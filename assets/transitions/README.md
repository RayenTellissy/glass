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
- `passes` (optional): up to 8 extra passes drawn in order before `fs`, for work at a
  reduced resolution (a blur, a pyramid for minification). See below.

### Extra passes

```json
"passes": [
  {"entry": "reduce", "scale": 0.5},
  {"entry": "strip", "scale": {"along": 0.25, "across": 1}, "min": 256}
]
```

- `entry`: a `@fragment` entry point in `shader.wgsl` other than `fs`; several passes may
  share one and tell themselves apart by `tr.pass_index`.
- `scale`: the pass's target size as a fraction (1/16 to 1) of the output's, one number
  for both axes or `along` and `across` the direction of travel (along is y for `up`
  and `down`, x otherwise); 1 when left out.
- `min`: the least size of the target on either axis in pixels, unless the output is
  smaller; small renders such as thumbnails then keep their detail.

Pass `i` draws into its own target; `fs` and every later pass read it as `pass<i>`
through `sample_pass(i, uv)` and `size_of_pass(i)`. While a pass draws, `tr.pass_index`
is its index (the pass count in `fs`), `tr.pass_size` its target's size, and `in.pos`
and `in.uv` are in its target; the passes after it read as a 1x1 placeholder. Every
entry point is checked at discovery and on every hot reload: an edit that drops one is
rejected and the last good shader stays.

## `shader.wgsl`

Compiled after `crates/render/shaders/common.wgsl` and
`crates/render/shaders/transition.wgsl`, which declares the uniform `tr` and the inputs A
(outgoing) and B (incoming), premultiplied linear light in the working space. The shader
defines `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` (and the entry points of
its extra passes) and must return exactly A at progress 0 and exactly B at progress 1.
See `transition.wgsl` for the uniform layout and the shared helpers: the
wipes' edge kernel and composite (`edge`, `wipe`) and the motion transitions' moved
frames (`frame_a`, `frame_b`, transparent black outside with anti-aliased edges), `over`,
`rotate` and `jitter`, and the stylised transitions' seeded noise (`seed_key`,
`hash_cell`, `value_noise`, `fbm`: integer hashing, so a seed always gives the same
pattern) and `field_edge` (the edge kernel over a thresholded field's screen slope).
