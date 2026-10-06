mod run;
mod view;

use crate::compiled::schedule::{End, Outcome, Segment};
use crate::compiled::table::Table;
use crate::compiled::typed::Literal;
use crate::compiled::{Body, CompiledGraph, Condition, Kind, Step};
use petgraph::prelude::NodeIndex;
use std::sync::{Arc, OnceLock, RwLock};
use view::{M, Masks, Node, Ref, Resolved, Shape, SlotId, Slots};
use zen_expression::lane::{Binding as Site, Columns, Dictionary, Kind as LaneKind, LaneProgram, Values};

type Bound = Arc<Result<Arc<SegPlan>, String>>;

pub(crate) struct Plan {
    roots: RwLock<Vec<(Arc<Layout>, Bound)>>,
}

#[derive(PartialEq, Eq, Clone)]
pub(crate) struct Layout(Vec<(Arc<str>, u8)>);

#[derive(Clone)]
pub(crate) enum Source {
    Input(usize),
    Op,
}

#[derive(Clone)]
pub(crate) struct SlotInfo {
    pub shape: Shape,
    pub source: Source,
}

#[derive(Clone, Copy)]
pub(crate) enum Program {
    Expression,
    Condition(usize),
}

pub(crate) struct LaneOp {
    pub node: NodeIndex,
    pub program: Program,
    pub reads: Vec<(Arc<str>, Ref)>,
    pub bound: Vec<Site>,
    pub outs: Vec<SlotId>,
    pub uniform: bool,
    pub stable: bool,
    pub specialized: OnceLock<Option<LaneProgram>>,
    pub own: Option<Box<Reduced>>,
}

pub(crate) struct Reduced {
    pub program: LaneProgram,
    pub entries: Box<[(Arc<str>, Arc<str>)]>,
}

pub(crate) struct TableOp {
    pub node: NodeIndex,
    pub reads: Vec<(Arc<str>, Option<Ref>)>,
    pub outs: Vec<SlotId>,
    pub present: Vec<M>,
    pub matched: M,
    pub mode: TableMode,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TableMode {
    First,
    Collected,
    Rows,
}

pub(crate) enum Op {
    Lane(Box<LaneOp>),
    Table(TableOp),
    Select { a: Ref, b: Ref, take: M, out: SlotId },
    Materialize { node: Node, out: SlotId },
    Extract { from: Ref, path: Vec<Arc<str>>, out: SlotId },
    Null { out: SlotId },
    Coalesce { from: Ref, value: Literal, out: SlotId },
    MergeRows { a: Node, b: Node, parts: [M; 3], present: M, out: SlotId },
}

pub(crate) enum Cond {
    Always,
    Never,
    Lane(Box<LaneOp>),
}

pub(crate) enum SegEnd {
    Finish(Vec<(Arc<str>, Ref)>),
    Switch {
        first: bool,
        ids: Vec<Arc<str>>,
        conditions: Vec<Cond>,
        children: RwLock<Vec<(Vec<u64>, Bound)>>,
    },
}

pub(crate) struct SegPlan {
    pub segment: Arc<Segment>,
    pub slots: Vec<SlotInfo>,
    pub masks: Masks,
    pub ops: Vec<Op>,
    pub end: SegEnd,
    views: Vec<(NodeIndex, Node)>,
}

struct Builder<'g> {
    graph: &'g CompiledGraph,
    masks: Masks,
    catalog: Catalog,
    views: Vec<(NodeIndex, Node)>,
    objects: Vec<(Node, Ref)>,
}

#[derive(Default)]
struct Catalog {
    slots: Vec<SlotInfo>,
    ops: Vec<Op>,
}

impl Catalog {
    fn slot(&mut self, shape: Shape) -> SlotId {
        self.slots.push(SlotInfo { shape, source: Source::Op });
        (self.slots.len() - 1) as SlotId
    }
}

impl Slots for Catalog {
    fn shape(&self, slot: SlotId) -> Shape {
        self.slots[slot as usize].shape
    }

    fn merge_rows(&mut self, a: Node, b: Node, parts: [M; 3], present: M) -> SlotId {
        let out = self.slot(Shape::Dyn);
        self.ops.push(Op::MergeRows { a, b, parts, present, out });
        out
    }

    fn select(&mut self, a: Ref, b: Ref, take: M, _present: M) -> SlotId {
        let shape = match (self.shape(a.slot), self.shape(b.slot)) {
            (x, y) if x == y => x,
            (Shape::Null, x) | (x, Shape::Null) if x.scalar() => x,
            _ => Shape::Unknown,
        };
        let out = self.slot(shape);
        self.ops.push(Op::Select { a, b, take, out });
        out
    }
}

impl<'g> Builder<'g> {
    fn slot(&mut self, shape: Shape) -> SlotId {
        self.catalog.slot(shape)
    }

    fn input(&mut self, layout: &Layout) -> Result<Node, String> {
        let mut root = Node::root(M::All);
        for (index, (path, tag)) in layout.0.iter().enumerate() {
            let shape = match tag {
                0 => Shape::Num,
                1 => Shape::Bool,
                2 => Shape::Text,
                _ => Shape::Dyn,
            };
            self.catalog.slots.push(SlotInfo {
                shape,
                source: Source::Input(index),
            });
            let slot = (self.catalog.slots.len() - 1) as SlotId;
            let present = self.masks.validity(slot);
            root.insert(path, Ref { slot, present }, M::All)?;
        }
        let mut fields = std::mem::take(&mut root.fields);
        for (_, child) in fields.iter_mut() {
            child.objects_from_leaves(&mut self.masks);
        }
        root.fields = fields;
        Ok(root)
    }

    fn read(&mut self, view: &Node, key: &str, base: Option<&str>) -> Result<Option<Ref>, String> {
        let rebased = match base {
            Some(base) => CompiledGraph::rebased(base, key).ok_or_else(|| format!("read {key} under {base}"))?,
            None => Arc::from(key),
        };
        let key = rebased.as_ref();
        let (view, key) = match key.strip_prefix("$nodes.") {
            Some(rest) => {
                let (name, path) = rest.split_once('.').unwrap_or((rest, ""));
                if !crate::ZEN_CONFIG.nodes_in_context.load(std::sync::atomic::Ordering::Relaxed) {
                    return Ok(None);
                }
                let named = self.graph.steps.iter().flatten().filter(|s| s.node.name.as_ref() == name).count();
                if named > 1 {
                    return Err(format!("duplicate node name {name}"));
                }
                let found = self
                    .views
                    .iter()
                    .rev()
                    .find(|(n, _)| self.graph.step_at(*n).is_some_and(|s| s.node.name.as_ref() == name))
                    .map(|(_, v)| v.clone());
                let Some(found) = found else {
                    return Ok(None);
                };
                if path.is_empty() {
                    return Err("whole $nodes value".into());
                }
                (found, path.to_string())
            }
            None => (view.clone(), key.to_string()),
        };
        let key = key.as_str();
        if key.starts_with('$') {
            return Err(format!("read {key} needs the row"));
        }
        match view.resolve(key, &self.catalog)? {
            Resolved::Leaf(leaf) => Ok(Some(leaf)),
            Resolved::Absent => Ok(None),
            Resolved::Object(node) => {
                if let Some((_, found)) = self.objects.iter().find(|(n, _)| *n == node) {
                    return Ok(Some(*found));
                }
                let present = node.obj.unwrap_or(M::None);
                let out = self.slot(Shape::Dyn);
                self.catalog.ops.push(Op::Materialize { node: node.clone(), out });
                let found = Ref { slot: out, present };
                self.objects.push((node, found));
                Ok(Some(found))
            }
            Resolved::Extract(from, path) => {
                let out = self.slot(Shape::Dyn);
                self.catalog.ops.push(Op::Extract { from, path, out });
                let present = self.masks.valued(out);
                Ok(Some(Ref { slot: out, present }))
            }
        }
    }

    fn lane(&mut self, node: NodeIndex, kind: Program, program: &LaneProgram, input: &Node, base: Option<&str>, outs: Vec<SlotId>) -> Result<LaneOp, String> {
        let p = program.program();
        if p.writes_env || p.chain || p.opaque() || p.rows {
            return Err("program needs rows".into());
        }
        let mut reads: Vec<(Arc<str>, Ref)> = Vec::new();
        let mut bound = Vec::with_capacity(p.site_keys.len());
        for key in &p.site_keys {
            let Some(key) = key.as_deref() else {
                bound.push(Site::Row);
                continue;
            };
            bound.push(match self.read(input, key, base)? {
                None => Site::Absent,
                Some(leaf) => match reads.iter().position(|(k, _)| k.as_ref() == key) {
                    Some(at) => Site::Column(at),
                    None => {
                        reads.push((Arc::from(key), leaf));
                        Site::Column(reads.len() - 1)
                    }
                },
            });
        }
        let stable = p.stable() && !p.opaque();
        let uniform = reads.is_empty() && stable;
        Ok(LaneOp {
            node,
            program: kind,
            reads,
            bound,
            outs,
            uniform,
            stable,
            specialized: OnceLock::new(),
            own: None,
        })
    }

    fn literal(text: &str) -> Option<Literal> {
        let text = text.trim();
        match text {
            "true" => return Some(Literal::Bool(true)),
            "false" => return Some(Literal::Bool(false)),
            _ => {}
        }
        let quoted = ['\'', '"']
            .iter()
            .find_map(|q| text.strip_prefix(*q).and_then(|t| t.strip_suffix(*q)))
            .filter(|inner| !inner.contains(['\\', '\'', '"', '$', '`']));
        match quoted {
            Some(inner) => Some(Literal::Str(Arc::from(inner))),
            None if text.chars().all(|c| c.is_ascii_digit() || c == '.') && text.starts_with(|c: char| c.is_ascii_digit()) => {
                text.parse::<rust_decimal::Decimal>().ok().map(Literal::Num)
            }
            None => None,
        }
    }

    fn coalesce(&mut self, input: &Node, source: &str) -> Result<Option<Node>, String> {
        let Some((left, right)) = source.split_once("??") else {
            return Ok(None);
        };
        let (Some(value), true) = (Self::literal(right), Self::path(left.trim())) else {
            return Ok(None);
        };
        let Resolved::Leaf(leaf) = input.resolve(left.trim(), &self.catalog)? else {
            return Ok(None);
        };
        let shape = match (&value, self.catalog.shape(leaf.slot)) {
            (Literal::Num(_), Shape::Num) => Shape::Num,
            (Literal::Str(_), Shape::Text) => Shape::Text,
            (Literal::Bool(_), Shape::Bool) => Shape::Bool,
            _ => return Ok(None),
        };
        if !self.masks.implied(leaf) {
            return Ok(None);
        }
        let out = self.slot(shape);
        self.catalog.ops.push(Op::Coalesce { from: leaf, value, out });
        Ok(Some(Node {
            leaf: Some(Ref { slot: out, present: M::All }),
            obj: None,
            fields: Vec::new(),
        }))
    }

    fn path(path: &str) -> bool {
        !matches!(path, "" | "true" | "false" | "null")
            && path.split('.').all(|segment| {
                segment.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
    }

    fn alias(&mut self, input: &Node, source: &str) -> Result<Option<Node>, String> {
        let path = source.trim();
        if !Self::path(path) {
            return self.coalesce(input, path);
        }
        Ok(match input.resolve(path, &self.catalog)? {
            Resolved::Leaf(leaf) if self.masks.implied(leaf) => Some(Node {
                leaf: Some(Ref { slot: leaf.slot, present: M::All }),
                obj: None,
                fields: Vec::new(),
            }),
            Resolved::Object(mut node) if node.leaf.is_none() => {
                let obj = node.obj.unwrap_or(M::None);
                if obj != M::All {
                    let null = self.slot(Shape::Null);
                    self.catalog.ops.push(Op::Null { out: null });
                    let missing = self.masks.not(obj);
                    node.leaf = Some(Ref { slot: null, present: missing });
                }
                Some(node)
            }
            _ => None,
        })
    }

    fn expression(&mut self, node: NodeIndex, step: &Step, input: &Node) -> Result<Node, String> {
        let Kind::Node {
            body: Body::Expression { program, keys, empty, entries, fragments, .. },
            transform,
        } = &step.kind
        else {
            return Err("not an expression".into());
        };
        if transform.input_field.is_some() && transform.base.is_none() {
            return Err("dynamic input field".into());
        }
        if matches!(&step.kind, Kind::Node { body: Body::Expression { walked: true, .. }, .. }) {
            return Err("dollar copies under overlapping keys".into());
        }
        if let Some(base) = &transform.base {
            if matches!(input.resolve(base, &self.catalog)?, Resolved::Leaf(_) | Resolved::Extract(..)) {
                return Err("input field may hold a scalar".into());
            }
        }
        let p = program.program();
        let regs: Vec<_> = match p.outputs.is_empty() {
            true => vec![p.out],
            false => p.outputs.clone(),
        };
        if regs.len() != keys.len() {
            return Err("output arity".into());
        }
        let aliasable = transform.base.is_none() && !p.opaque() && !p.writes_env && !p.chain;
        let mut aliases: Vec<Option<Node>> = Vec::with_capacity(keys.len());
        for (key, fragment) in keys.iter().zip(fragments.iter()) {
            let root = format!("$.{}", key.split('.').next().unwrap_or_default());
            let read = entries.iter().any(|(_, v)| v.contains(root.as_str()));
            aliases.push(match aliasable && !read && fragments.len() == keys.len() {
                true => self.alias(input, fragment)?,
                false => None,
            });
        }
        let reduced = match aliases.iter().any(Option::is_some) {
            true => {
                let kept: Vec<((Arc<str>, Arc<str>), Arc<str>)> = entries
                    .iter()
                    .zip(fragments.iter())
                    .zip(&aliases)
                    .filter(|(_, alias)| alias.is_none())
                    .map(|((entry, fragment), _)| (entry.clone(), fragment.clone()))
                    .collect();
                let refs: Vec<(&str, &str)> = kept.iter().map(|((k, _), v)| (k.as_ref(), v.as_ref())).collect();
                match kept.is_empty() {
                    true => None,
                    false => Some(Reduced {
                        program: LaneProgram::compile_many(&refs, true).map_err(|e| e.to_string())?,
                        entries: kept.into_iter().map(|(entry, _)| entry).collect(),
                    }),
                }
            }
            false => None,
        };
        let lane_program = reduced.as_ref().map_or(program.as_ref(), |r| &r.program);
        let lp = lane_program.program();
        let lane_regs: Vec<_> = match lp.outputs.is_empty() {
            true => vec![lp.out],
            false => lp.outputs.clone(),
        };
        let mut lane_regs = lane_regs.into_iter();
        let mut out = Node::root(M::All);
        let mut outs = Vec::new();
        for (((key, empty), (_, source)), alias) in keys.iter().zip(empty.iter()).zip(entries.iter()).zip(aliases.iter_mut()) {
            if keys.iter().filter(|k| *k == key).count() > 1 {
                return Err("duplicate expression key".into());
            }
            if key.split('.').any(str::is_empty) {
                return Err("empty key segment".into());
            }
            let path = match &transform.output_path {
                Some(prefix) => format!("{prefix}.{key}"),
                None => key.to_string(),
            };
            if let Some(alias) = alias.take() {
                out.insert_node(&path, alias, M::All)?;
                continue;
            }
            let reg = lane_regs.next().ok_or("output arity")?;
            let shape = match (lp.kinds[reg as usize], source.trim() == "null") {
                (_, true) => Shape::Null,
                (LaneKind::Num, _) => Shape::Num,
                (LaneKind::Bool, _) => Shape::Bool,
                (LaneKind::Str, _) => Shape::Text,
                _ => Shape::Dyn,
            };
            let slot = self.slot(shape);
            outs.push(slot);
            match empty {
                true => out.insert_object(&path, M::All)?,
                false => out.insert(&path, Ref { slot, present: M::All }, M::All)?,
            }
        }
        if !outs.is_empty() {
            let mut op = self.lane(node, Program::Expression, lane_program, input, transform.base.as_deref(), outs)?;
            op.own = reduced.map(Box::new);
            self.catalog.ops.push(Op::Lane(Box::new(op)));
        }
        match transform.pass_through {
            true => Node::merge(input, &out, &mut self.masks, &mut self.catalog),
            false => Ok(out),
        }
    }

    fn table(&mut self, node: NodeIndex, step: &Step, table: &Table, input: &Node) -> Result<Node, String> {
        let Kind::Node { transform, .. } = &step.kind else {
            return Err("not a table".into());
        };
        if transform.input_field.is_some() {
            return Err("table input field".into());
        }
        let mode = match (table.first_hit(), table.collects(), &transform.output_path) {
            (true, _, _) => TableMode::First,
            (false, true, _) => TableMode::Collected,
            (false, false, Some(_)) => TableMode::Rows,
            _ => return Err("collect table".into()),
        };
        let mut reads: Vec<(Arc<str>, Option<Ref>)> = Vec::new();
        for program in table.programs() {
            let p = program.program();
            if p.opaque() || p.writes_env || p.chain {
                return Err("table program needs rows".into());
            }
            for key in p.site_keys.iter() {
                let Some(key) = key.as_deref() else {
                    continue;
                };
                if key.starts_with("$.") {
                    return Err(format!("table cell reads {key} through its column"));
                }
                if key == "$" || reads.iter().any(|(k, _)| k.as_ref() == key) {
                    continue;
                }
                if key.starts_with('$') && !key.starts_with("$nodes.") {
                    return Err(format!("table read {key}"));
                }
                let leaf = self.read(input, key, None)?;
                reads.push((Arc::from(key), leaf));
            }
        }
        let out = match mode {
            TableMode::Rows => {
                let slot = self.slot(Shape::Dyn);
                self.catalog.ops.push(Op::Table(TableOp {
                    node,
                    reads,
                    outs: vec![slot],
                    present: Vec::new(),
                    matched: M::All,
                    mode,
                }));
                let mut out = Node::root(M::All);
                if let Some(path) = &transform.output_path {
                    out.insert(path, Ref { slot, present: M::All }, M::All)?;
                }
                out
            }
            _ => {
                let paths = table.output_paths();
                let mut out = Node::root(M::None);
                let mut outs = Vec::with_capacity(paths.len());
                let mut present = Vec::with_capacity(paths.len());
                for path in paths.iter() {
                    let shape = match table.slot_kind(path) {
                        Some(0) => Shape::Num,
                        Some(1) => Shape::Bool,
                        Some(2) => Shape::Text,
                        _ => Shape::Unknown,
                    };
                    let slot = self.slot(shape);
                    let mask = self.masks.produced();
                    outs.push(slot);
                    present.push(mask);
                    out.insert(path, Ref { slot, present: mask }, M::None)?;
                }
                let matched = self.masks.produced();
                out.objects_from_leaves(&mut self.masks);
                out.obj = Some(matched);
                self.catalog.ops.push(Op::Table(TableOp {
                    node,
                    reads,
                    outs,
                    present,
                    matched,
                    mode,
                }));
                match &transform.output_path {
                    None => out,
                    Some(path) => {
                        let null = self.slot(Shape::Dyn);
                        self.catalog.ops.push(Op::Null { out: null });
                        let missing = self.masks.not(matched);
                        out.leaf = Some(Ref { slot: null, present: missing });
                        Node::wrap(out, path)
                    }
                }
            }
        };
        match transform.pass_through {
            true => Node::merge(input, &out, &mut self.masks, &mut self.catalog),
            false => Ok(out),
        }
    }

    fn view(&self, node: NodeIndex) -> Option<Node> {
        self.views.iter().rev().find(|(n, _)| *n == node).map(|(_, v)| v.clone())
    }

    fn data(&mut self, parents: &[NodeIndex]) -> Result<Node, String> {
        let mut parents = parents.iter().filter_map(|p| self.view(*p)).collect::<Vec<_>>().into_iter();
        let mut acc = match parents.next() {
            Some(head) => head.head(),
            None => Node::root(M::All),
        };
        for parent in parents {
            acc = Node::merge(&acc, &parent, &mut self.masks, &mut self.catalog)?;
        }
        Ok(acc)
    }

    fn segment(mut self, segment: Arc<Segment>, layout: &Layout) -> Result<SegPlan, String> {
        for event in segment.events.iter() {
            let step = self.graph.step_at(event.node).ok_or("missing step")?;
            let data = match step.kind {
                Kind::Input => self.input(layout)?,
                _ => self.data(&event.parents)?,
            };
            let output = match &step.kind {
                Kind::Input | Kind::Output | Kind::Switch { .. } => data,
                Kind::Node { body: Body::Expression { .. }, .. } => self.expression(event.node, step, &data)?,
                Kind::Node { body: Body::Table(table), .. } => self.table(event.node, step, table, &data)?,
                Kind::Host => return Err("host".into()),
            };
            self.views.push((event.node, output));
        }
        let end = match &segment.end {
            End::Finish(endings) => {
                let mut acc = Node::root(M::All);
                for ending in endings.iter() {
                    if let Some(view) = self.view(*ending) {
                        acc = Node::merge(&acc, &view, &mut self.masks, &mut self.catalog)?;
                    }
                }
                let mut output = Vec::new();
                acc.leaves("", &mut output);
                if output.iter().any(|(path, _)| path.split('.').any(|s| s == "$nodes")) {
                    return Err("reserved output".into());
                }
                SegEnd::Finish(output)
            }
            End::Switch { node, parents, .. } => {
                let step = self.graph.step_at(*node).ok_or("missing switch")?;
                let Kind::Switch { first, conditions } = &step.kind else {
                    return Err("not a switch".into());
                };
                let input = self.data(parents)?;
                let mut built = Vec::with_capacity(conditions.len());
                for (index, (_, condition)) in conditions.iter().enumerate() {
                    built.push(match condition {
                        Condition::Always => Cond::Always,
                        Condition::Never => Cond::Never,
                        Condition::Program(program) => {
                            let out = self.slot(Shape::Bool);
                            let op = self.lane(*node, Program::Condition(index), program, &input, None, vec![out])?;
                            match op.uniform && program.program().timeless() {
                                true => {
                                    let scope = zen_expression::Scope::default();
                                    let truth = zen_expression::lane::LaneRunner::new().evaluate_one(program, &scope);
                                    match truth {
                                        Ok(zen_expression::Variable::Bool(true)) => Cond::Always,
                                        _ => Cond::Never,
                                    }
                                }
                                false => Cond::Lane(Box::new(op)),
                            }
                        }
                    });
                }
                SegEnd::Switch {
                    first: *first,
                    ids: conditions.iter().map(|(id, _)| id.clone()).collect(),
                    conditions: built,
                    children: RwLock::new(Vec::new()),
                }
            }
        };
        Ok(SegPlan {
            segment,
            slots: self.catalog.slots,
            masks: self.masks,
            ops: self.catalog.ops,
            end,
            views: self.views,
        })
    }
}

impl SegPlan {
    pub fn child(&self, graph: &CompiledGraph, layout: &Layout, key: &[u64]) -> Bound {
        let SegEnd::Switch { children, ids, .. } = &self.end else {
            return Arc::new(Err("not a switch".into()));
        };
        if let Ok(cache) = children.read() {
            if let Some((_, child)) = cache.iter().find(|(k, _)| k.as_slice() == key) {
                return child.clone();
            }
        }
        let outcome: Outcome = ids
            .iter()
            .enumerate()
            .filter(|(index, _)| key.get(index / 64).is_some_and(|word| word >> (index % 64) & 1 == 1))
            .map(|(_, id)| id.clone())
            .collect();
        let segment = graph.child(&self.segment, outcome);
        let builder = Builder {
            graph,
            masks: self.masks.clone(),
            catalog: Catalog {
                slots: self.slots.clone(),
                ops: Vec::new(),
            },
            views: self.views.clone(),
            objects: Vec::new(),
        };
        let built: Bound = Arc::new(builder.segment(segment, layout).map(Arc::new));
        if let Ok(mut cache) = children.write() {
            cache.push((key.to_vec(), built.clone()));
        }
        built
    }
}

impl Plan {
    pub fn analyze(graph: &CompiledGraph) -> Result<Plan, String> {
        for step in graph.steps.iter().flatten() {
            Self::analyze_step(step)?;
        }
        Ok(Plan {
            roots: RwLock::new(Vec::new()),
        })
    }

    pub fn analyze_step(step: &Step) -> Result<(), String> {
        let schema = step.schema && !matches!(step.kind, Kind::Input);
        let nodes = step.nodes && step.statics.is_none();
        if nodes || step.root || step.dynamic || schema {
            return Err(format!("node {} needs rows", step.node.name));
        }
        match &step.kind {
            Kind::Input | Kind::Output | Kind::Switch { .. } => Ok(()),
            Kind::Node { transform, .. } => match transform.looped {
                true => Err("loop".into()),
                false => Ok(()),
            },
            Kind::Host => Err("host".into()),
        }
    }

    pub fn verdict(&self, graph: &CompiledGraph, columns: &Columns) -> Result<(), String> {
        let (_, root) = self.root(graph, columns).ok_or("layout")?;
        match root.as_ref() {
            Ok(_) => Ok(()),
            Err(reason) => Err(reason.clone()),
        }
    }

    fn tag(column: &zen_expression::lane::Column) -> u8 {
        match column.values {
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => 0,
            Values::Bool { .. } => 1,
            Values::Utf8 { .. } | Values::Text { .. } | Values::LargeUtf8 { .. } | Values::Strs(_) => 2,
            Values::Dict { values, .. } => match values {
                Dictionary::Scaled { .. } => 0,
                Dictionary::Bool { .. } => 1,
                Dictionary::Text { .. } => 2,
                _ => 3,
            },
            Values::List { .. } | Values::Any(_) => 3,
        }
    }

    fn layout(columns: &Columns) -> Option<Layout> {
        columns
            .columns
            .iter()
            .map(|(path, column)| (!path.is_empty()).then(|| (Arc::from(*path), Self::tag(column))))
            .collect::<Option<Vec<_>>>()
            .map(Layout)
    }

    fn matches(layout: &Layout, columns: &Columns) -> bool {
        layout.0.len() == columns.columns.len()
            && layout
                .0
                .iter()
                .zip(&columns.columns)
                .all(|((path, tag), (name, column))| path.as_ref() == *name && *tag == Self::tag(column))
    }

    pub fn root(&self, graph: &CompiledGraph, columns: &Columns) -> Option<(Arc<Layout>, Bound)> {
        if let Ok(cache) = self.roots.read() {
            if let Some((layout, binding)) = cache.iter().find(|(l, _)| Self::matches(l, columns)) {
                return Some((layout.clone(), binding.clone()));
            }
        }
        let layout = Arc::new(Self::layout(columns)?);
        let builder = Builder {
            graph,
            masks: Masks::default(),
            catalog: Catalog::default(),
            views: Vec::new(),
            objects: Vec::new(),
        };
        let built: Bound = Arc::new(builder.segment(graph.root.clone(), &layout).map(Arc::new));
        if let Ok(mut cache) = self.roots.write() {
            if cache.len() >= 16 {
                cache.remove(0);
            }
            cache.push((layout.clone(), built.clone()));
        }
        Some((layout, built))
    }
}
