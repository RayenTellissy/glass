//! The per-frame render graph. The engine describes a frame as a DAG of [`Node`]s
//! (sources, gamut conversion, compositing, output encoding and so on) wired through
//! [`Port`]s, with imported textures as extra inputs and exported textures as the
//! results. [`Graph::compile`] sorts the nodes topologically, culls nodes no export
//! depends on, derives every intermediate's size and format, and assigns intermediates to
//! physical textures, aliasing those whose lifetimes do not overlap. [`CompiledGraph::execute`]
//! records one pass (or a few) per node into a command encoder, timing each with
//! [`GpuProfiler`], and submits.
//!
//! ```ignore
//! let mut g = Graph::new();
//! let a = g.add(YuvSourceNode::uploaded(&frame_a, working), &[]);
//! let b = g.import("cached frame", &cached_view);
//! let c = g.add(GamutNode::new(Primaries::DisplayP3, Primaries::Bt709), &[b]);
//! let layers = vec![Layer::blended(BlendMode::Normal), Layer::blended(BlendMode::Screen)];
//! let out = g.add(CompositeNode::new(canvas, false, layers), &[a.out(0), c.out(0)]);
//! g.export(out.out(0), &target_view);
//! let compiled = g.compile()?;
//! compiled.execute(&renderer, &mut pool, Some(&mut profiler));
//! ```
//!
//! Intermediates are `Rgba16Float` holding linear light with premultiplied alpha
//! ([`TextureDesc::working`]); only output nodes write other formats.

mod pool;

use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub use pool::{POOL_USAGE, TextureDesc, TexturePool};

use crate::profiler::GpuProfiler;
use crate::renderer::{Effect, Renderer};

/// A unit of GPU work in the graph.
pub trait Node {
    /// Name for profiling and debugging.
    fn label(&self) -> &str;

    /// How many input textures the node reads.
    fn input_count(&self) -> usize;

    /// The textures the node writes, given its inputs'.
    fn outputs(&self, inputs: &[TextureDesc]) -> Vec<TextureDesc>;

    /// The node's parameters as name and value, for the debug panel.
    fn params(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// Records the node's passes. Every output must be fully overwritten; its previous
    /// contents are undefined (the texture may have held another intermediate).
    fn record(
        &self,
        cx: &mut NodeContext<'_>,
        inputs: &[&wgpu::TextureView],
        outputs: &[&wgpu::TextureView],
    );
}

/// What a node records with.
pub struct NodeContext<'a> {
    pub renderer: &'a Renderer,
    pub encoder: &'a mut wgpu::CommandEncoder,
    timestamps: Option<wgpu::RenderPassTimestampWrites<'a>>,
}

impl<'a> NodeContext<'a> {
    /// The timestamp writes for this node's pass. The first call gets them (when
    /// profiling); a node with several passes passes them to its main one.
    pub fn timestamps(&mut self) -> Option<wgpu::RenderPassTimestampWrites<'a>> {
        self.timestamps.take()
    }

    /// Draws a full-screen effect into `target`, timed.
    pub fn draw(&mut self, effect: &Effect<'_>, target: &wgpu::TextureView) {
        let timestamps = self.timestamps();
        self.renderer.draw(self.encoder, effect, target, timestamps);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// The node's `slot`th output.
    pub fn out(self, slot: u8) -> Port {
        Port(PortKind::Node { node: self.0, slot })
    }
}

/// A texture in the graph: a node output or an imported texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Port(PortKind);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PortKind {
    Node { node: u32, slot: u8 },
    Import(u32),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("node {node} ({label}) has input {input} unconnected")]
    Unconnected {
        node: u32,
        label: String,
        input: usize,
    },
    #[error("node {node} ({label}) reads a port that does not exist")]
    BadPort { node: u32, label: String },
    #[error("the graph has a cycle through node {0}")]
    Cycle(u32),
    #[error("the graph exports nothing")]
    NoExports,
    #[error("export {0} is not a node output")]
    ExportNotNodeOutput(usize),
    #[error("export {index} is {expected:?} but its target is {actual:?}")]
    ExportMismatch {
        index: usize,
        expected: TextureDesc,
        actual: TextureDesc,
    },
    #[error("a port is exported twice")]
    DuplicateExport,
}

struct NodeEntry<'a> {
    node: Box<dyn Node + 'a>,
    inputs: Vec<Option<Port>>,
}

struct Import<'a> {
    label: String,
    view: &'a wgpu::TextureView,
    desc: TextureDesc,
}

/// A frame's graph under construction.
#[derive(Default)]
pub struct Graph<'a> {
    nodes: Vec<NodeEntry<'a>>,
    imports: Vec<Import<'a>>,
    exports: Vec<(Port, &'a wgpu::TextureView)>,
}

impl<'a> Graph<'a> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
        }
    }

    /// Adds a node reading `inputs` in order.
    pub fn add(&mut self, node: impl Node + 'a, inputs: &[Port]) -> NodeId {
        let id = self.add_node(node);
        for (i, port) in inputs.iter().enumerate() {
            self.connect(*port, id, i);
        }
        id
    }

    /// Adds a node with its inputs unconnected; see [`Self::connect`].
    pub fn add_node(&mut self, node: impl Node + 'a) -> NodeId {
        let inputs = vec![None; node.input_count()];
        self.nodes.push(NodeEntry {
            node: Box::new(node),
            inputs,
        });
        NodeId(self.nodes.len() as u32 - 1)
    }

    /// Feeds `port` into input `index` of `node`. Nodes may be added in any order;
    /// [`Self::compile`] sorts them.
    pub fn connect(&mut self, port: Port, node: NodeId, index: usize) {
        if let Some(entry) = self.nodes.get_mut(node.0 as usize) {
            if entry.inputs.len() <= index {
                entry.inputs.resize(index + 1, None);
            }
            entry.inputs[index] = Some(port);
        }
    }

    /// An existing texture as a graph input (a cached frame, a still, a previous result).
    pub fn import(&mut self, label: &str, view: &'a wgpu::TextureView) -> Port {
        self.imports.push(Import {
            label: label.to_owned(),
            view,
            desc: TextureDesc::of(view.texture()),
        });
        Port(PortKind::Import(self.imports.len() as u32 - 1))
    }

    /// Makes the node output `port` render straight into `target`; the target's size and
    /// format must match what the node produces.
    pub fn export(&mut self, port: Port, target: &'a wgpu::TextureView) {
        self.exports.push((port, target));
    }

    pub fn compile(self) -> Result<CompiledGraph<'a>, GraphError> {
        compile(self)
    }
}

/// Where a resource lives during execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Pool slot `n`.
    Slot(usize),
    Import(usize),
    Export(usize),
}

/// One texture the compiled graph uses.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceInfo {
    pub desc: TextureDesc,
    pub placement: Placement,
    /// Positions in the pass list of the producing and the last reading pass.
    pub first: usize,
    pub last: usize,
}

struct Step<'a> {
    node: Box<dyn Node + 'a>,
    inputs: Vec<usize>,
    outputs: Vec<usize>,
}

/// Memory and pass statistics from compilation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileStats {
    pub passes: usize,
    pub culled: usize,
    /// Intermediates the graph describes.
    pub transient_textures: usize,
    /// Pool textures after aliasing.
    pub physical_textures: usize,
    pub transient_bytes: u64,
    pub physical_bytes: u64,
}

impl CompileStats {
    /// VRAM aliasing saves compared with one texture per intermediate.
    pub fn saved_bytes(&self) -> u64 {
        self.transient_bytes - self.physical_bytes
    }
}

/// A graph ready to execute.
pub struct CompiledGraph<'a> {
    steps: Vec<Step<'a>>,
    resources: Vec<ResourceInfo>,
    slots: Vec<TextureDesc>,
    imports: Vec<Import<'a>>,
    exports: Vec<&'a wgpu::TextureView>,
    stats: CompileStats,
}

fn compile(graph: Graph<'_>) -> Result<CompiledGraph<'_>, GraphError> {
    let Graph {
        nodes,
        imports,
        exports,
    } = graph;
    let n = nodes.len();
    let label = |i: usize| nodes[i].node.label().to_owned();

    // Validate inputs and collect node-to-node edges.
    let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indegree = vec![0usize; n];
    for (i, entry) in nodes.iter().enumerate() {
        if entry.inputs.len() != entry.node.input_count() {
            return Err(GraphError::Unconnected {
                node: i as u32,
                label: label(i),
                input: entry.inputs.len().min(entry.node.input_count()),
            });
        }
        for (input, port) in entry.inputs.iter().enumerate() {
            match port {
                None => {
                    return Err(GraphError::Unconnected {
                        node: i as u32,
                        label: label(i),
                        input,
                    });
                }
                Some(Port(PortKind::Node { node, .. })) => {
                    let from = *node as usize;
                    if from >= n {
                        return Err(GraphError::BadPort {
                            node: i as u32,
                            label: label(i),
                        });
                    }
                    consumers[from].push(i);
                    indegree[i] += 1;
                }
                Some(Port(PortKind::Import(k))) => {
                    if *k as usize >= imports.len() {
                        return Err(GraphError::BadPort {
                            node: i as u32,
                            label: label(i),
                        });
                    }
                }
            }
        }
    }

    // Kahn's algorithm, always taking the earliest-added ready node: the order is stable,
    // and a layer's chain runs before the next layer's source, which keeps few
    // intermediates alive at once.
    let mut order = Vec::with_capacity(n);
    let mut ready: BinaryHeap<Reverse<usize>> =
        (0..n).filter(|&i| indegree[i] == 0).map(Reverse).collect();
    let mut remaining = indegree;
    while let Some(Reverse(i)) = ready.pop() {
        order.push(i);
        for &c in &consumers[i] {
            remaining[c] -= 1;
            if remaining[c] == 0 {
                ready.push(Reverse(c));
            }
        }
    }
    if order.len() != n {
        let stuck = (0..n).find(|&i| remaining[i] > 0).unwrap_or(0);
        return Err(GraphError::Cycle(stuck as u32));
    }

    // Cull everything no export depends on.
    if exports.is_empty() {
        return Err(GraphError::NoExports);
    }
    let mut live = vec![false; n];
    let mut stack = Vec::new();
    for (index, (port, _)) in exports.iter().enumerate() {
        match port.0 {
            PortKind::Node { node, .. } if (node as usize) < n => stack.push(node as usize),
            _ => return Err(GraphError::ExportNotNodeOutput(index)),
        }
    }
    for (a, (pa, _)) in exports.iter().enumerate() {
        if exports[..a].iter().any(|(pb, _)| pb == pa) {
            return Err(GraphError::DuplicateExport);
        }
    }
    while let Some(i) = stack.pop() {
        if std::mem::replace(&mut live[i], true) {
            continue;
        }
        for port in nodes[i].inputs.iter().flatten() {
            if let PortKind::Node { node, .. } = port.0 {
                stack.push(node as usize);
            }
        }
    }
    let order: Vec<usize> = order.into_iter().filter(|&i| live[i]).collect();
    let culled = n - order.len();

    // Resources: imports first, then every live node's outputs.
    let mut resources: Vec<ResourceInfo> = imports
        .iter()
        .enumerate()
        .map(|(k, import)| ResourceInfo {
            desc: import.desc,
            placement: Placement::Import(k),
            first: 0,
            last: 0,
        })
        .collect();
    let mut output_base = vec![usize::MAX; n];
    let mut output_count = vec![0usize; n];
    let mut step_inputs: Vec<Vec<usize>> = Vec::with_capacity(order.len());
    for (position, &i) in order.iter().enumerate() {
        let mut inputs = Vec::new();
        for port in nodes[i].inputs.iter().flatten() {
            let resource = match port.0 {
                PortKind::Import(k) => k as usize,
                PortKind::Node { node, slot } => {
                    let node = node as usize;
                    if usize::from(slot) >= output_count[node] {
                        return Err(GraphError::BadPort {
                            node: i as u32,
                            label: label(i),
                        });
                    }
                    output_base[node] + usize::from(slot)
                }
            };
            resources[resource].last = position;
            inputs.push(resource);
        }
        let descs: Vec<TextureDesc> = inputs.iter().map(|&r| resources[r].desc).collect();
        let outputs = nodes[i].node.outputs(&descs);
        output_base[i] = resources.len();
        output_count[i] = outputs.len();
        for desc in outputs {
            resources.push(ResourceInfo {
                desc,
                placement: Placement::Slot(usize::MAX),
                first: position,
                last: position,
            });
        }
        step_inputs.push(inputs);
    }

    // Exports render straight into their targets.
    let mut export_views = Vec::with_capacity(exports.len());
    for (index, (port, view)) in exports.iter().enumerate() {
        let PortKind::Node { node, slot } = port.0 else {
            return Err(GraphError::ExportNotNodeOutput(index));
        };
        let node = node as usize;
        if usize::from(slot) >= output_count[node] {
            return Err(GraphError::ExportNotNodeOutput(index));
        }
        let resource = &mut resources[output_base[node] + usize::from(slot)];
        let actual = TextureDesc::of(view.texture());
        if actual != resource.desc {
            return Err(GraphError::ExportMismatch {
                index,
                expected: resource.desc,
                actual,
            });
        }
        resource.placement = Placement::Export(index);
        export_views.push(*view);
    }

    // Alias: give each intermediate, in order of first use, a slot of the same
    // description whose previous occupant was last read by an earlier pass.
    let mut slots: Vec<TextureDesc> = Vec::new();
    let mut slot_free_after: Vec<usize> = Vec::new();
    let mut stats = CompileStats {
        passes: order.len(),
        culled,
        ..Default::default()
    };
    for resource in resources.iter_mut() {
        if resource.placement != Placement::Slot(usize::MAX) {
            continue;
        }
        stats.transient_textures += 1;
        stats.transient_bytes += resource.desc.bytes();
        let reuse = (0..slots.len())
            .filter(|&s| slots[s] == resource.desc && slot_free_after[s] < resource.first)
            .min_by_key(|&s| slot_free_after[s]);
        let slot = match reuse {
            Some(s) => s,
            None => {
                slots.push(resource.desc);
                slot_free_after.push(0);
                stats.physical_textures += 1;
                stats.physical_bytes += resource.desc.bytes();
                slots.len() - 1
            }
        };
        slot_free_after[slot] = resource.last;
        resource.placement = Placement::Slot(slot);
    }

    let mut nodes: Vec<Option<NodeEntry<'_>>> = nodes.into_iter().map(Some).collect();
    let steps = order
        .iter()
        .zip(step_inputs)
        .map(|(&i, inputs)| {
            let entry = nodes[i].take().expect("each node is scheduled once");
            Step {
                node: entry.node,
                inputs,
                outputs: (output_base[i]..output_base[i] + output_count[i]).collect(),
            }
        })
        .collect();
    Ok(CompiledGraph {
        steps,
        resources,
        slots,
        imports,
        exports: export_views,
        stats,
    })
}

impl<'a> CompiledGraph<'a> {
    pub fn stats(&self) -> CompileStats {
        self.stats
    }

    /// Every texture the graph touches, imports first.
    pub fn resources(&self) -> &[ResourceInfo] {
        &self.resources
    }

    /// Pass labels in execution order.
    pub fn pass_labels(&self) -> Vec<&str> {
        self.steps.iter().map(|s| s.node.label()).collect()
    }

    /// Records every pass into `encoder`, taking intermediates from `pool`, after
    /// picking up any shader reloaded since the last frame
    /// ([`Renderer::begin_frame`]).
    pub fn record(
        &self,
        renderer: &Renderer,
        pool: &mut TexturePool,
        encoder: &mut wgpu::CommandEncoder,
        mut profiler: Option<&mut GpuProfiler>,
    ) {
        renderer.begin_frame();
        let slot_views = pool.acquire(renderer.device(), &self.slots);
        let view = |r: usize| -> &wgpu::TextureView {
            match self.resources[r].placement {
                Placement::Slot(s) => &slot_views[s],
                Placement::Import(k) => self.imports[k].view,
                Placement::Export(k) => self.exports[k],
            }
        };
        for step in &self.steps {
            let inputs: Vec<&wgpu::TextureView> = step.inputs.iter().map(|&r| view(r)).collect();
            let outputs: Vec<&wgpu::TextureView> = step.outputs.iter().map(|&r| view(r)).collect();
            let timestamps = profiler
                .as_deref_mut()
                .and_then(|p| p.pass(step.node.label()));
            let mut cx = NodeContext {
                renderer,
                encoder,
                timestamps,
            };
            step.node.record(&mut cx, &inputs, &outputs);
        }
    }

    /// Records, submits and returns the submission; timings arrive through `profiler`.
    pub fn execute(
        &self,
        renderer: &Renderer,
        pool: &mut TexturePool,
        mut profiler: Option<&mut GpuProfiler>,
    ) -> wgpu::SubmissionIndex {
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("render graph"),
                });
        if let Some(p) = profiler.as_deref_mut() {
            p.begin_frame();
        }
        self.record(renderer, pool, &mut encoder, profiler.as_deref_mut());
        if let Some(p) = profiler.as_deref_mut() {
            p.end_frame(&mut encoder);
        }
        let index = renderer.queue().submit([encoder.finish()]);
        if let Some(p) = profiler {
            p.after_submit();
        }
        index
    }

    /// A readable description of passes and their textures, for the debug panel.
    pub fn describe(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let name = |r: usize| match self.resources[r].placement {
            Placement::Slot(s) => format!("t{s}"),
            Placement::Import(k) => format!("import:{}", self.imports[k].label),
            Placement::Export(k) => format!("export{k}"),
        };
        for (i, step) in self.steps.iter().enumerate() {
            let inputs: Vec<String> = step.inputs.iter().map(|&r| name(r)).collect();
            let outputs: Vec<String> = step.outputs.iter().map(|&r| name(r)).collect();
            let _ = write!(
                out,
                "{i}: {} [{}] -> [{}]",
                step.node.label(),
                inputs.join(", "),
                outputs.join(", ")
            );
            for (k, v) in step.node.params() {
                let _ = write!(out, " {k}={v}");
            }
            out.push('\n');
        }
        let s = self.stats;
        let _ = write!(
            out,
            "{} intermediates in {} textures: {:.1} of {:.1} MB",
            s.transient_textures,
            s.physical_textures,
            s.physical_bytes as f64 / 1e6,
            s.transient_bytes as f64 / 1e6
        );
        out
    }
}
