pub(crate) mod census;
mod run;
pub(crate) use run::PlanOutcome;
pub(crate) mod refusal;
pub(crate) mod subgraphs;
mod typed;
mod view;

use crate::compiled::core::segments::Children;
use crate::compiled::graph::call::Call;
use crate::compiled::graph::schedule::{End, Outcome, Segment};
use crate::compiled::graph::{Body, CompiledGraph, Condition, Kind, Step};
use crate::compiled::leaf::Literal;
use crate::compiled::table::Table;
use crate::model::{DecisionNodeKind, GraphContent};
use petgraph::prelude::NodeIndex;
use refusal::{ChildRefusal, Hosted, Needs, Refusal};
use std::sync::{Arc, OnceLock, RwLock};
use subgraphs::{Closure, Subgraphs};
use view::{Masks, Node, Ref, Resolved, Shape, SlotId, Slots, M};
use zen_expression::lane::{
    Binding as Site, Columns, Dictionary, Kind as LaneKind, LaneProgram, Values,
};
use zen_types::decision::TransformExecutionMode;

type Bound = Arc<Result<Arc<SegPlan>, Refusal>>;
struct Kept {
    entry: (Arc<str>, Arc<str>),
    fragment: Arc<str>,
}

struct Keyed {
    view: Node,
    keys: Vec<(Arc<str>, SlotId, M)>,
}

pub(crate) struct Plan {
    roots: RwLock<Vec<Root>>,
}

struct Root {
    layout: Arc<Layout>,
    bound: Bound,
    closure: Closure,
}

#[derive(PartialEq, Eq, Clone)]
pub(crate) struct Layout(Vec<(Arc<str>, Shape)>);

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
    Field,
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
    pub sources: Vec<(Arc<str>, Arc<str>)>,
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
    Typed {
        op: Box<typed::TypedOp>,
        units: Vec<Op>,
    },
    Deferred {
        op: Box<typed::Deferred>,
        units: Vec<Op>,
    },
    Select {
        a: Ref,
        b: Ref,
        take: M,
        out: SlotId,
    },
    Materialize {
        node: Node,
        out: SlotId,
    },
    Extract {
        from: Ref,
        path: Vec<Arc<str>>,
        out: SlotId,
    },
    Null {
        out: SlotId,
    },
    Coalesce {
        from: Ref,
        value: Literal,
        out: SlotId,
    },
    MergeRows {
        a: Node,
        b: Node,
        parts: [M; 3],
        present: M,
        out: SlotId,
    },
    Rows(Box<RowsOp>),
    Join {
        inputs: Vec<Ref>,
        out: SlotId,
        mode: JoinMode,
    },
    Loop {
        node: NodeIndex,
        list: Ref,
        out: SlotId,
    },
    Child(Box<ChildOp>),
}

pub(crate) struct ChildKey {
    pub path: Arc<str>,
    pub child: Ref,
    pub out: SlotId,
    pub present: M,
}

pub(crate) struct ChildOp {
    pub node: NodeIndex,
    pub view: Node,
    pub input: Node,
    pub nodes: Option<Vec<(Arc<str>, Ref)>>,
    pub content: Arc<GraphContent>,
    pub root: Arc<SegPlan>,
    pub layout: Arc<Layout>,
    pub keys: Vec<ChildKey>,
    pub record: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinMode {
    Strict,
    Lenient,
    Ending,
}

pub(crate) struct RowsOp {
    pub node: NodeIndex,
    pub view: Node,
    pub nodes: Option<Vec<(Arc<str>, Ref)>>,
    pub out: SlotId,
    pub keys: Vec<(Arc<str>, SlotId, M)>,
    pub reason: Arc<str>,
}

pub(crate) struct RowsCond {
    pub index: usize,
    pub node: NodeIndex,
    pub input: Ref,
    pub nodes: Option<Vec<(Arc<str>, Ref)>>,
    pub reason: Arc<str>,
}

pub(crate) enum Cond {
    Always,
    Never,
    Lane(Box<LaneOp>),
    Rows(Box<RowsCond>),
}

pub(crate) enum SegEnd {
    Finish(Vec<(Arc<str>, Ref)>),
    Switch {
        first: bool,
        ids: Vec<Arc<str>>,
        conditions: Vec<Cond>,
        children: Children<Vec<u64>, Bound>,
    },
}

pub(crate) struct SegPlan {
    pub segment: Arc<Segment>,
    pub slots: Vec<SlotInfo>,
    pub masks: Masks,
    pub ops: Vec<Op>,
    pub end: SegEnd,
    pub hosted: Vec<Refusal>,
    views: Vec<(NodeIndex, Node)>,
    nested: Nested,
}

#[derive(Clone, Default)]
struct Nested {
    subgraphs: Option<Arc<Subgraphs>>,
    stack: Arc<[Arc<str>]>,
}

struct Builder<'g> {
    graph: &'g CompiledGraph,
    masks: Masks,
    catalog: Catalog,
    views: Vec<(NodeIndex, Node)>,
    objects: Vec<(Node, Ref)>,
    hosted: Vec<Refusal>,
    null: Option<SlotId>,
    nested: Nested,
}

#[derive(Default)]
struct Catalog {
    slots: Vec<SlotInfo>,
    ops: Vec<Op>,
}

impl Catalog {
    fn slot(&mut self, shape: Shape) -> SlotId {
        self.slots.push(SlotInfo {
            shape,
            source: Source::Op,
        });
        (self.slots.len() - 1) as SlotId
    }
}

impl Slots for Catalog {
    fn shape(&self, slot: SlotId) -> Shape {
        self.slots[slot as usize].shape
    }

    fn merge_rows(&mut self, a: Node, b: Node, parts: [M; 3], present: M) -> SlotId {
        let out = self.slot(Shape::Dyn);
        self.ops.push(Op::MergeRows {
            a,
            b,
            parts,
            present,
            out,
        });
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

    fn input(&mut self, layout: &Layout) -> Result<Node, Refusal> {
        let mut root = Node::root(M::All);
        for (index, (path, shape)) in layout.0.iter().enumerate() {
            self.catalog.slots.push(SlotInfo {
                shape: *shape,
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

    fn read(&mut self, view: &Node, key: &str, base: Option<&str>) -> Result<Option<Ref>, Refusal> {
        let rebased = match base {
            Some(base) => CompiledGraph::rebased(base, key).ok_or_else(|| Refusal::Rebase {
                key: Arc::from(key),
                base: Arc::from(base),
            })?,
            None => Arc::from(key),
        };
        let key = rebased.as_ref();
        let (view, key) = match key.strip_prefix("$nodes.") {
            Some(rest) => {
                let (name, path) = rest.split_once('.').unwrap_or((rest, ""));
                if !crate::ZEN_CONFIG
                    .nodes_in_context
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return Ok(None);
                }
                let named = self
                    .graph
                    .steps
                    .iter()
                    .flatten()
                    .filter(|s| s.node.name.as_ref() == name)
                    .count();
                if named > 1 {
                    return Err(Refusal::DuplicateName(Arc::from(name)));
                }
                let found = self
                    .views
                    .iter()
                    .rev()
                    .find(|(n, _)| {
                        self.graph
                            .step_at(*n)
                            .is_some_and(|s| s.node.name.as_ref() == name)
                    })
                    .map(|(_, v)| v.clone());
                let Some(found) = found else {
                    return Ok(None);
                };
                if path.is_empty() {
                    return Err(Refusal::WholeNodes);
                }
                (found, path.to_string())
            }
            None => (view.clone(), key.to_string()),
        };
        let key = key.as_str();
        if key.starts_with('$') {
            return Err(Refusal::RowRead(Arc::from(key)));
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
                self.catalog.ops.push(Op::Materialize {
                    node: node.clone(),
                    out,
                });
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

    fn lane(
        &mut self,
        node: NodeIndex,
        kind: Program,
        program: &LaneProgram,
        input: &Node,
        base: Option<&str>,
        outs: Vec<SlotId>,
    ) -> Result<LaneOp, Refusal> {
        let needs = Needs::of(program);
        if needs.any() {
            return Err(Refusal::Program(needs));
        }
        let p = program.program();
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
            sources: Vec::new(),
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
            None if text.chars().all(|c| c.is_ascii_digit() || c == '.')
                && text.starts_with(|c: char| c.is_ascii_digit()) =>
            {
                text.parse::<rust_decimal::Decimal>().ok().map(Literal::Num)
            }
            None => None,
        }
    }

    fn coalesce(&mut self, input: &Node, source: &str) -> Result<Option<Node>, Refusal> {
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
        self.catalog.ops.push(Op::Coalesce {
            from: leaf,
            value,
            out,
        });
        Ok(Some(Node {
            leaf: Some(Ref {
                slot: out,
                present: M::All,
            }),
            obj: None,
            fields: Vec::new(),
        }))
    }

    fn path(path: &str) -> bool {
        !matches!(path, "" | "true" | "false" | "null")
            && path.split('.').all(|segment| {
                segment
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
    }

    fn alias(&mut self, input: &Node, source: &str) -> Result<Option<Node>, Refusal> {
        let path = source.trim();
        if !Self::path(path) {
            return self.coalesce(input, path);
        }
        Ok(match input.resolve(path, &self.catalog)? {
            Resolved::Leaf(leaf) if self.masks.implied(leaf) => Some(Node {
                leaf: Some(Ref {
                    slot: leaf.slot,
                    present: M::All,
                }),
                obj: None,
                fields: Vec::new(),
            }),
            Resolved::Object(mut node) if node.leaf.is_none() => {
                let obj = node.obj.unwrap_or(M::None);
                if obj != M::All {
                    let null = match self
                        .null
                        .filter(|slot| (*slot as usize) < self.catalog.slots.len())
                    {
                        Some(slot) => slot,
                        None => {
                            let slot = self.slot(Shape::Null);
                            self.catalog.ops.push(Op::Null { out: slot });
                            self.null = Some(slot);
                            slot
                        }
                    };
                    let missing = self.masks.not(obj);
                    node.leaf = Some(Ref {
                        slot: null,
                        present: missing,
                    });
                }
                Some(node)
            }
            _ => None,
        })
    }

    fn expression(&mut self, node: NodeIndex, step: &Step, input: &Node) -> Result<Node, Refusal> {
        let Kind::Node {
            body:
                Body::Expression {
                    program,
                    keys,
                    empty,
                    entries,
                    fragments,
                    ..
                },
            transform,
        } = &step.kind
        else {
            return Err(Refusal::NotExpression);
        };
        if transform.input_field.is_some() && transform.base.is_none() {
            return Err(Refusal::DynamicInputField);
        }
        if matches!(
            &step.kind,
            Kind::Node {
                body: Body::Expression { walked: true, .. },
                ..
            }
        ) {
            return Err(Refusal::OverlappingCopies);
        }
        if let Some(base) = &transform.base {
            if matches!(
                input.resolve(base, &self.catalog)?,
                Resolved::Leaf(_) | Resolved::Extract(..)
            ) {
                return Err(Refusal::ScalarInputField);
            }
        }
        let p = program.program();
        let regs: Vec<_> = match p.outputs.is_empty() {
            true => vec![p.out],
            false => p.outputs.clone(),
        };
        if regs.len() != keys.len() {
            return Err(Refusal::OutputArity);
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
                let kept: Vec<Kept> = entries
                    .iter()
                    .zip(fragments.iter())
                    .zip(&aliases)
                    .filter(|(_, alias)| alias.is_none())
                    .map(|((entry, fragment), _)| Kept {
                        entry: entry.clone(),
                        fragment: fragment.clone(),
                    })
                    .collect();
                let refs: Vec<(&str, &str)> = kept
                    .iter()
                    .map(|kept| (kept.entry.0.as_ref(), kept.fragment.as_ref()))
                    .collect();
                match kept.is_empty() {
                    true => None,
                    false => Some(Reduced {
                        program: LaneProgram::compile_many(&refs, true)
                            .map_err(|e| Refusal::Uncompiled(Arc::from(e.to_string())))?,
                        entries: kept.into_iter().map(|kept| kept.entry).collect(),
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
        let mut sources = Vec::new();
        for ((((key, empty), (_, source)), alias), fragment) in keys
            .iter()
            .zip(empty.iter())
            .zip(entries.iter())
            .zip(aliases.iter_mut())
            .zip(fragments.iter())
        {
            if keys.iter().filter(|k| *k == key).count() > 1 {
                return Err(Refusal::DuplicateExpressionKey);
            }
            if key.split('.').any(str::is_empty) {
                return Err(Refusal::EmptyKeySegment);
            }
            let path = match &transform.output_path {
                Some(prefix) => format!("{prefix}.{key}"),
                None => key.to_string(),
            };
            if let Some(alias) = alias.take() {
                out.insert_node(&path, alias, M::All)?;
                continue;
            }
            let reg = lane_regs.next().ok_or(Refusal::OutputArity)?;
            let shape = match (lp.kinds[reg as usize], source.trim() == "null") {
                (_, true) => Shape::Null,
                (LaneKind::Num, _) => Shape::Num,
                (LaneKind::Bool, _) => Shape::Bool,
                (LaneKind::Str, _) => Shape::Text,
                _ => Shape::Dyn,
            };
            let slot = self.slot(shape);
            outs.push(slot);
            sources.push((key.clone(), fragment.clone()));
            match empty {
                true => out.insert_object(&path, M::All)?,
                false => out.insert(
                    &path,
                    Ref {
                        slot,
                        present: M::All,
                    },
                    M::All,
                )?,
            }
        }
        if !outs.is_empty() {
            let mut op = self.lane(
                node,
                Program::Expression,
                lane_program,
                input,
                transform.base.as_deref(),
                outs,
            )?;
            op.own = reduced.map(Box::new);
            op.sources = sources;
            self.catalog.ops.push(Op::Lane(Box::new(op)));
        }
        match transform.pass_through {
            true => Node::merge(input, &out, &mut self.masks, &mut self.catalog),
            false => Ok(out),
        }
    }

    fn table(
        &mut self,
        node: NodeIndex,
        step: &Step,
        table: &Table,
        input: &Node,
    ) -> Result<Node, Refusal> {
        let Kind::Node { transform, .. } = &step.kind else {
            return Err(Refusal::NotTable);
        };
        if transform.input_field.is_some() {
            return Err(Refusal::TableInputField);
        }
        let mode = match (table.first_hit(), table.collects(), &transform.output_path) {
            (true, _, _) => TableMode::First,
            (false, true, _) => TableMode::Collected,
            (false, false, _) if transform.output_path.is_some() || !transform.pass_through => {
                TableMode::Rows
            }
            _ => return Err(Refusal::CollectTable),
        };
        let mut reads: Vec<(Arc<str>, Option<Ref>)> = Vec::new();
        for program in table.programs() {
            let p = program.program();
            if p.opaque() || p.writes_env || p.chain {
                return Err(Refusal::TableNeedsRows);
            }
            for key in p.site_keys.iter() {
                let Some(key) = key.as_deref() else {
                    continue;
                };
                if key.starts_with("$.") {
                    return Err(Refusal::TableCellRead(Arc::from(key)));
                }
                if key == "$" || reads.iter().any(|(k, _)| k.as_ref() == key) {
                    continue;
                }
                if key.starts_with('$') && !key.starts_with("$nodes.") {
                    return Err(Refusal::TableRead(Arc::from(key)));
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
                match &transform.output_path {
                    Some(path) => {
                        let mut out = Node::root(M::All);
                        out.insert(
                            path,
                            Ref {
                                slot,
                                present: M::All,
                            },
                            M::All,
                        )?;
                        out
                    }
                    None => Node {
                        leaf: Some(Ref {
                            slot,
                            present: M::All,
                        }),
                        obj: None,
                        fields: Vec::new(),
                    },
                }
            }
            _ => {
                let paths = table.output_paths();
                let mut out = Node::root(M::None);
                let mut outs = Vec::with_capacity(paths.len());
                let mut present = Vec::with_capacity(paths.len());
                for path in paths.iter() {
                    let shape = table.slot_kind(path).map_or(Shape::Unknown, Shape::from);
                    let slot = self.slot(shape);
                    let mask = self.masks.produced();
                    outs.push(slot);
                    present.push(mask);
                    out.insert(
                        path,
                        Ref {
                            slot,
                            present: mask,
                        },
                        M::None,
                    )?;
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
                        out.leaf = Some(Ref {
                            slot: null,
                            present: missing,
                        });
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

    fn whole(&mut self, view: &Node) -> Ref {
        if let Some(leaf) = view.leaf.filter(|leaf| leaf.present == M::All) {
            return leaf;
        }
        if let Some((_, found)) = self.objects.iter().find(|(n, _)| n == view) {
            return Ref {
                slot: found.slot,
                present: M::All,
            };
        }
        let out = self.slot(Shape::Dyn);
        self.catalog.ops.push(Op::Materialize {
            node: view.clone(),
            out,
        });
        self.objects.push((
            view.clone(),
            Ref {
                slot: out,
                present: view.obj.unwrap_or(M::None),
            },
        ));
        Ref {
            slot: out,
            present: M::All,
        }
    }

    fn nodes(&mut self, step: &Step, visible: &[NodeIndex]) -> Option<Vec<(Arc<str>, Ref)>> {
        if !step.nodes
            || !crate::ZEN_CONFIG
                .nodes_in_context
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        let views: Vec<(Arc<str>, Node)> = visible
            .iter()
            .filter_map(|nid| {
                Some((
                    self.graph.step_at(*nid)?.node.name.clone(),
                    self.view(*nid)?,
                ))
            })
            .collect();
        Some(
            views
                .into_iter()
                .map(|(name, view)| (name, self.whole(&view)))
                .collect(),
        )
    }

    fn child(
        &mut self,
        node: NodeIndex,
        step: &Step,
        input: &Node,
        visible: &[NodeIndex],
    ) -> Result<Node, Refusal> {
        let DecisionNodeKind::DecisionNode { content } = &step.node.kind else {
            return Err(Refusal::Host(Hosted::Decision));
        };
        let subgraphs = self
            .nested
            .subgraphs
            .clone()
            .ok_or(Refusal::Child(ChildRefusal::Unresolved))?;
        if self.nested.stack.contains(&content.key) {
            return Err(Refusal::Child(ChildRefusal::Cycle));
        }
        let child = subgraphs
            .get(&content.key)
            .cloned()
            .ok_or(Refusal::Child(ChildRefusal::Unresolved))?;
        let graph = Call::compiled(&child).map_err(|_| Refusal::Child(ChildRefusal::Unresolved))?;
        if Self::validated(graph, &child) {
            return Err(Refusal::Child(ChildRefusal::Schemas));
        }
        let attributes = &content.transform_attributes;
        if !matches!(attributes.execution_mode, TransformExecutionMode::Single) {
            return Err(Refusal::Child(ChildRefusal::Loop));
        }
        let view = match attributes.input_field.as_deref() {
            None => input.clone(),
            Some(field) if Self::plain(field) => match input.resolve(field, &self.catalog)? {
                Resolved::Object(view) => view,
                _ => return Err(Refusal::Child(ChildRefusal::InputField)),
            },
            Some(_) => return Err(Refusal::Child(ChildRefusal::InputField)),
        };
        if !Self::tree(&view, true) {
            return Err(Refusal::Child(ChildRefusal::Whole));
        }
        let mut leaves = Vec::new();
        view.leaves("", &mut leaves);
        let layout = Layout(
            leaves
                .iter()
                .map(|(path, r)| {
                    let shape = match self.catalog.shape(r.slot) {
                        shape @ (Shape::Num | Shape::Bool | Shape::Text) => shape,
                        _ => Shape::Dyn,
                    };
                    (path.clone(), shape)
                })
                .collect(),
        );
        let stack: Arc<[Arc<str>]> = self
            .nested
            .stack
            .iter()
            .cloned()
            .chain([content.key.clone()])
            .collect();
        let (layout, bound) = graph.plan().nested(graph, layout, &subgraphs, stack);
        let root = bound
            .as_ref()
            .as_ref()
            .map_err(|refusal| Refusal::ChildPlan(Box::new(refusal.clone())))?
            .clone();
        let SegEnd::Finish(refs) = &root.end else {
            return Err(Refusal::Child(ChildRefusal::Branches));
        };
        if root.ops.iter().any(|op| {
            matches!(
                op,
                Op::Join {
                    mode: JoinMode::Strict,
                    ..
                }
            )
        }) {
            return Err(Refusal::Child(ChildRefusal::Strict));
        }
        let mut keys = Vec::with_capacity(refs.len());
        let out = match refs.as_slice() {
            [(path, r)] if path.is_empty() => {
                if attributes.pass_through {
                    return Err(Refusal::Child(ChildRefusal::Whole));
                }
                let slot = self.slot(root.slots[r.slot as usize].shape);
                let present = self.masks.produced();
                keys.push(ChildKey {
                    path: path.clone(),
                    child: *r,
                    out: slot,
                    present,
                });
                Node {
                    leaf: Some(Ref { slot, present }),
                    obj: None,
                    fields: Vec::new(),
                }
            }
            refs => {
                let mut out = Node::root(M::All);
                for (path, r) in refs {
                    if path.is_empty() {
                        return Err(Refusal::Child(ChildRefusal::Whole));
                    }
                    let slot = self.slot(root.slots[r.slot as usize].shape);
                    let present = self.masks.produced();
                    out.insert(path, Ref { slot, present }, M::None)?;
                    keys.push(ChildKey {
                        path: path.clone(),
                        child: *r,
                        out: slot,
                        present,
                    });
                }
                out.objects_from_leaves(&mut self.masks);
                out.obj = Some(M::All);
                out
            }
        };
        let record = Self::effects(graph, &subgraphs);
        let nodes = self.nodes(step, visible);
        self.catalog.ops.push(Op::Child(Box::new(ChildOp {
            node,
            view,
            input: input.clone(),
            nodes,
            content: child,
            root,
            layout,
            keys,
            record,
        })));
        let out = match attributes.output_path.as_deref() {
            Some(path) => Node::wrap(out, path),
            None => out,
        };
        match attributes.pass_through {
            true => Node::merge(input, &out, &mut self.masks, &mut self.catalog),
            false => Ok(out),
        }
    }

    fn validated(graph: &CompiledGraph, content: &GraphContent) -> bool {
        graph
            .steps
            .iter()
            .flatten()
            .any(|step| match &step.node.kind {
                DecisionNodeKind::InputNode { content: input } => {
                    input.schema.is_some()
                        || content
                            .resolved_schemas
                            .as_ref()
                            .is_some_and(|resolved| resolved.contains_key(&step.node.id))
                }
                DecisionNodeKind::OutputNode { .. } => false,
                _ => content
                    .resolved_schemas
                    .as_ref()
                    .is_some_and(|resolved| resolved.contains_key(&step.node.id)),
            })
    }

    fn effects(graph: &CompiledGraph, subgraphs: &Subgraphs) -> bool {
        let impure = |graph: &CompiledGraph| {
            graph.steps.iter().flatten().any(|step| {
                matches!(
                    step.node.kind,
                    DecisionNodeKind::FunctionNode { .. } | DecisionNodeKind::CustomNode { .. }
                )
            })
        };
        impure(graph)
            || subgraphs
                .closure(graph)
                .iter()
                .any(|(_, content)| Call::compiled(content).map_or(true, impure))
    }

    fn plain(field: &str) -> bool {
        !field.is_empty()
            && field.split('.').all(|segment| {
                segment
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
    }

    fn tree(node: &Node, root: bool) -> bool {
        match (node.leaf, root) {
            (Some(_), true) => false,
            (Some(_), false) => node.fields.is_empty(),
            (None, _) => node
                .fields
                .iter()
                .all(|(_, child)| Self::tree(child, false)),
        }
    }

    fn rows(
        &mut self,
        node: NodeIndex,
        step: &Step,
        input: &Node,
        visible: &[NodeIndex],
        reason: Arc<str>,
    ) -> Node {
        let nodes = self.nodes(step, visible);
        let out = self.slot(Shape::Dyn);
        let keyed = self.attempt(|b| b.keyed(step));
        let Keyed { view, keys } = keyed.unwrap_or_else(|_| {
            let leaf = Node {
                leaf: Some(Ref {
                    slot: out,
                    present: M::All,
                }),
                obj: None,
                fields: Vec::new(),
            };
            Keyed {
                view: leaf,
                keys: Vec::new(),
            }
        });
        self.catalog.ops.push(Op::Rows(Box::new(RowsOp {
            node,
            view: input.clone(),
            nodes,
            out,
            keys,
            reason,
        })));
        let merged = match &step.kind {
            Kind::Node { transform, .. } if transform.pass_through && view.leaf.is_none() => self
                .attempt(|b| Node::merge(input, &view, &mut b.masks, &mut b.catalog))
                .ok(),
            _ => Some(view),
        };
        merged.unwrap_or_else(|| {
            if let Some(Op::Rows(op)) = self.catalog.ops.last_mut() {
                op.keys.clear();
            }
            Node {
                leaf: Some(Ref {
                    slot: out,
                    present: M::All,
                }),
                obj: None,
                fields: Vec::new(),
            }
        })
    }

    fn keyed(&mut self, step: &Step) -> Result<Keyed, Refusal> {
        let (keys, empty, transform) = match &step.kind {
            Kind::Node {
                body: Body::Expression { keys, empty, .. },
                transform,
            } => (keys, empty, transform),
            Kind::Node {
                body: Body::Table(table),
                transform,
            } if table.first_hit()
                && transform.output_path.is_none()
                && transform.input_field.is_none()
                && !transform.looped =>
            {
                let mut out = Node::root(M::All);
                let mut slots = Vec::new();
                for path in table.output_paths().iter() {
                    if path.is_empty() || path.contains('$') || path.split('.').any(str::is_empty) {
                        return Err(Refusal::Opaque);
                    }
                    let slot = self.slot(Shape::Dyn);
                    let present = self.masks.produced();
                    out.insert(path, Ref { slot, present }, M::None)?;
                    slots.push((path.clone(), slot, present));
                }
                out.objects_from_leaves(&mut self.masks);
                out.obj = Some(M::All);
                return Ok(Keyed {
                    view: out,
                    keys: slots,
                });
            }
            _ => return Err(Refusal::Opaque),
        };
        if transform.looped
            || keys.is_empty()
            || keys
                .iter()
                .any(|k| k.is_empty() || k.contains('$') || k.split('.').any(str::is_empty))
        {
            return Err(Refusal::Opaque);
        }
        let mut out = Node::root(M::All);
        let mut slots = Vec::with_capacity(keys.len());
        for (key, empty) in keys.iter().zip(empty.iter()) {
            if keys.iter().filter(|k| *k == key).count() > 1 {
                return Err(Refusal::DuplicateKey);
            }
            let path: Arc<str> = match &transform.output_path {
                Some(prefix) => Arc::from(format!("{prefix}.{key}")),
                None => key.clone(),
            };
            match empty {
                true => out.insert_object(&path, M::All)?,
                false => {
                    let slot = self.slot(Shape::Dyn);
                    out.insert(
                        &path,
                        Ref {
                            slot,
                            present: M::All,
                        },
                        M::All,
                    )?;
                    slots.push((path, slot, M::All));
                }
            }
        }
        Ok(Keyed {
            view: out,
            keys: slots,
        })
    }

    fn attempt<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let (slots, ops, objects, masks) = (
            self.catalog.slots.len(),
            self.catalog.ops.len(),
            self.objects.len(),
            self.masks.clone(),
        );
        f(self).inspect_err(|_| {
            self.catalog.slots.truncate(slots);
            self.catalog.ops.truncate(ops);
            self.objects.truncate(objects);
            self.masks = masks;
        })
    }

    fn reason(step: &Step) -> Refusal {
        if let Kind::Host(Some(why)) = &step.kind {
            return Refusal::Uncompiled(why.clone());
        }
        Refusal::Host(match &step.node.kind {
            crate::model::DecisionNodeKind::FunctionNode { .. } => Hosted::Function,
            crate::model::DecisionNodeKind::DecisionNode { .. } => Hosted::Decision,
            crate::model::DecisionNodeKind::CustomNode { .. } => Hosted::Custom,
            crate::model::DecisionNodeKind::OutputNode { .. } => Hosted::OutputSchema,
            _ => Hosted::Node,
        })
    }

    fn looped(&mut self, node: NodeIndex, step: &Step, input: &Node) -> Result<Node, Refusal> {
        let Kind::Node { transform, .. } = &step.kind else {
            return Err(Refusal::NotLoop);
        };
        let (Some(path), Some((program, _)), false) =
            (&transform.output_path, &transform.input_field, step.nodes)
        else {
            return Err(Refusal::Loop);
        };
        let list = match program.path() {
            Some(source) => self
                .read(input, source, None)?
                .ok_or(Refusal::LoopSourceAbsent)?,
            None => {
                let slot = self.slot(Shape::Dyn);
                let op = self.lane(node, Program::Field, program, input, None, vec![slot])?;
                self.catalog.ops.push(Op::Lane(Box::new(op)));
                Ref {
                    slot,
                    present: M::All,
                }
            }
        };
        let out = self.slot(Shape::List);
        self.catalog.ops.push(Op::Loop { node, list, out });
        let leaf = Node {
            leaf: Some(Ref {
                slot: out,
                present: M::All,
            }),
            obj: None,
            fields: Vec::new(),
        };
        let output = Node::wrap(leaf, path);
        match transform.pass_through {
            true => Node::merge(input, &output, &mut self.masks, &mut self.catalog),
            false => Ok(output),
        }
    }

    fn view(&self, node: NodeIndex) -> Option<Node> {
        self.views
            .iter()
            .rev()
            .find(|(n, _)| *n == node)
            .map(|(_, v)| v.clone())
    }

    fn join(&mut self, views: &[Node], mode: JoinMode) -> Node {
        let inputs = views.iter().map(|view| self.whole(view)).collect();
        let out = self.slot(Shape::Dyn);
        self.catalog.ops.push(Op::Join { inputs, out, mode });
        Node {
            leaf: Some(Ref {
                slot: out,
                present: M::All,
            }),
            obj: None,
            fields: Vec::new(),
        }
    }

    fn relax(&mut self, data: &Node) {
        let Some(leaf) = data.leaf else {
            return;
        };
        for op in self.catalog.ops.iter_mut().rev() {
            if let Op::Join { out, mode, .. } = op {
                if *out == leaf.slot {
                    if *mode == JoinMode::Strict {
                        *mode = JoinMode::Lenient;
                    }
                    return;
                }
            }
        }
    }

    fn data(&mut self, parents: &[NodeIndex]) -> Result<Node, Refusal> {
        let views: Vec<Node> = parents.iter().filter_map(|p| self.view(*p)).collect();
        if views.iter().any(|view| view.leaf.is_some()) {
            return Ok(self.join(&views, JoinMode::Strict));
        }
        let mut parents = views.into_iter();
        let mut acc = match parents.next() {
            Some(head) => head.head(),
            None => Node::root(M::All),
        };
        for parent in parents {
            acc = Node::merge(&acc, &parent, &mut self.masks, &mut self.catalog)?;
        }
        Ok(acc)
    }

    fn segment(mut self, segment: Arc<Segment>, layout: &Layout) -> Result<SegPlan, Refusal> {
        for event in segment.events.iter() {
            let step = self.graph.step_at(event.node).ok_or(Refusal::MissingStep)?;
            let data = match step.kind {
                Kind::Input => self.input(layout)?,
                _ => self.data(&event.parents)?,
            };
            let planned = match &step.kind {
                Kind::Input | Kind::Switch { .. } => Ok(data.clone()),
                Kind::Output if !step.schema => {
                    self.relax(&data);
                    Ok(data.clone())
                }
                Kind::Host(_)
                    if self.nested.subgraphs.is_some()
                        && matches!(step.node.kind, DecisionNodeKind::DecisionNode { .. }) =>
                {
                    self.attempt(|b| b.child(event.node, step, &data, &event.visible))
                }
                Kind::Output | Kind::Host(_) => Err(Self::reason(step)),
                Kind::Node { .. } => match Plan::analyze_step(step) {
                    Err(reason) => Err(reason),
                    Ok(()) => match &step.kind {
                        Kind::Node { transform, .. } if transform.looped => {
                            self.attempt(|b| b.looped(event.node, step, &data))
                        }
                        Kind::Node {
                            body: Body::Table(table),
                            ..
                        } => self.attempt(|b| b.table(event.node, step, table, &data)),
                        _ => self.attempt(|b| b.expression(event.node, step, &data)),
                    },
                },
            };
            let output = match planned {
                Ok(output) => output,
                Err(reason) => {
                    self.relax(&data);
                    let hosted = self.rows(
                        event.node,
                        step,
                        &data,
                        &event.visible,
                        Arc::from(reason.to_string()),
                    );
                    self.hosted.push(reason);
                    match (&step.kind, data.leaf.is_none()) {
                        (Kind::Output, true) => data.clone(),
                        _ => hosted,
                    }
                }
            };
            self.views.push((event.node, output));
        }
        let end = match &segment.end {
            End::Finish(endings) => {
                let views: Vec<Node> = endings
                    .iter()
                    .filter_map(|ending| self.view(*ending))
                    .collect();
                let mut acc = Node::root(M::All);
                match views.iter().any(|view| view.leaf.is_some()) {
                    true => acc = self.join(&views, JoinMode::Ending),
                    false => {
                        for view in &views {
                            acc = Node::merge(&acc, view, &mut self.masks, &mut self.catalog)?;
                        }
                    }
                }
                self.typed();
                let mut output = Vec::new();
                acc.leaves("", &mut output);
                if output
                    .iter()
                    .any(|(path, _)| path.split('.').any(|s| s == "$nodes"))
                {
                    return Err(Refusal::ReservedOutput);
                }
                SegEnd::Finish(output)
            }
            End::Switch {
                node,
                parents,
                visible,
                ..
            } => {
                self.typed();
                let step = self.graph.step_at(*node).ok_or(Refusal::MissingSwitch)?;
                let Kind::Switch { first, conditions } = &step.kind else {
                    return Err(Refusal::NotSwitch);
                };
                let input = self.data(parents)?;
                let mut built = Vec::with_capacity(conditions.len());
                for (index, (_, condition)) in conditions.iter().enumerate() {
                    built.push(match condition {
                        Condition::Always => Cond::Always,
                        Condition::Invalid(_) => Cond::Never,
                        Condition::Program(program) => {
                            let lane = self.attempt(|b| {
                                let out = b.slot(Shape::Bool);
                                b.lane(
                                    *node,
                                    Program::Condition(index),
                                    program,
                                    &input,
                                    None,
                                    vec![out],
                                )
                            });
                            let op = match lane {
                                Ok(op) => op,
                                Err(refusal) => {
                                    let refusal = Refusal::Condition(Box::new(refusal));
                                    let reason = Arc::from(refusal.to_string());
                                    self.hosted.push(refusal);
                                    let whole = self.whole(&input);
                                    let nodes = self.nodes(step, visible);
                                    built.push(Cond::Rows(Box::new(RowsCond {
                                        index,
                                        node: *node,
                                        input: whole,
                                        nodes,
                                        reason,
                                    })));
                                    continue;
                                }
                            };
                            match op.uniform && program.program().timeless() {
                                true => {
                                    let scope = zen_expression::Scope::default();
                                    let truth = zen_expression::lane::LaneRunner::new()
                                        .evaluate_one(program, &scope);
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
                if built.iter().all(|cond| !matches!(cond, Cond::Lane(_))) {
                    self.relax(&input);
                }
                SegEnd::Switch {
                    first: *first,
                    ids: conditions.iter().map(|(id, _)| id.clone()).collect(),
                    conditions: built,
                    children: Children::new(),
                }
            }
        };
        Ok(SegPlan {
            segment,
            slots: self.catalog.slots,
            masks: self.masks,
            ops: self.catalog.ops,
            end,
            hosted: self.hosted,
            views: self.views,
            nested: self.nested,
        })
    }
}

impl SegPlan {
    fn collect(&self, hosted: &mut Vec<Refusal>) -> Result<(), Refusal> {
        hosted.extend(self.hosted.iter().cloned());
        if let SegEnd::Switch { children, .. } = &self.end {
            children.each(|child| {
                child
                    .as_ref()
                    .as_ref()
                    .map_err(Refusal::clone)?
                    .collect(hosted)
            })?;
        }
        Ok(())
    }

    pub fn child(&self, graph: &CompiledGraph, layout: &Layout, key: &[u64]) -> Bound {
        let SegEnd::Switch { children, ids, .. } = &self.end else {
            return Arc::new(Err(Refusal::NotSwitch));
        };
        children.get(key, || self.branch(graph, layout, ids, key))
    }

    fn branch(
        &self,
        graph: &CompiledGraph,
        layout: &Layout,
        ids: &[Arc<str>],
        key: &[u64],
    ) -> Bound {
        let outcome: Outcome = ids
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                key.get(index / 64)
                    .is_some_and(|word| word >> (index % 64) & 1 == 1)
            })
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
            hosted: Vec::new(),
            null: None,
            nested: self.nested.clone(),
        };
        Arc::new(builder.segment(segment, layout).map(Arc::new))
    }
}

impl Plan {
    pub fn analyze(_graph: &CompiledGraph) -> Plan {
        Plan {
            roots: RwLock::new(Vec::new()),
        }
    }

    pub fn analyze_step(step: &Step) -> Result<(), Refusal> {
        let schema = step.schema && !matches!(step.kind, Kind::Input);
        let nodes = step.nodes && step.statics.is_none();
        if nodes || step.root || step.dynamic || schema {
            return Err(match (nodes, step.root, step.dynamic) {
                (true, _, _) => Refusal::Nodes,
                (_, true, _) => Refusal::Root,
                (_, _, true) => Self::dynamic(step),
                _ => Refusal::Schema,
            });
        }
        match &step.kind {
            Kind::Input | Kind::Output | Kind::Switch { .. } => Ok(()),
            Kind::Node { .. } => Ok(()),
            Kind::Host(_) => Err(Builder::reason(step)),
        }
    }

    fn dynamic(step: &Step) -> Refusal {
        let mut programs: Vec<&LaneProgram> = Vec::new();
        match &step.kind {
            Kind::Node { body, transform } => {
                match body {
                    Body::Expression { program, .. } => programs.push(program),
                    Body::Table(table) => programs.extend(table.programs()),
                }
                programs.extend(transform.input_field.iter().map(|(p, _)| p));
            }
            Kind::Switch { conditions, .. } => {
                programs.extend(conditions.iter().filter_map(|(_, c)| match c {
                    Condition::Program(p) => Some(p.as_ref()),
                    _ => None,
                }))
            }
            _ => {}
        }
        Refusal::Dynamic(
            programs
                .iter()
                .map(|p| Needs::of(p))
                .filter(Needs::dynamic)
                .collect(),
        )
    }

    pub fn verdict(&self, graph: &CompiledGraph, columns: &Columns) -> Result<(), Refusal> {
        let (_, root) = self
            .root(graph, columns, None, &[])
            .ok_or(Refusal::Layout)?;
        let plan = root.as_ref().as_ref().map_err(Refusal::clone)?;
        let mut hosted = Vec::new();
        plan.collect(&mut hosted)?;
        hosted.sort();
        hosted.dedup();
        match hosted.is_empty() {
            true => Ok(()),
            false => Err(Refusal::Mixed(hosted)),
        }
    }

    fn tag(column: &zen_expression::lane::Column) -> Shape {
        match column.values {
            Values::Dec(_) | Values::Scaled { .. } | Values::I64(_) | Values::F64(_) => Shape::Num,
            Values::Bool { .. } => Shape::Bool,
            Values::Utf8 { .. }
            | Values::Text { .. }
            | Values::LargeUtf8 { .. }
            | Values::Strs(_) => Shape::Text,
            Values::Dict { values, .. } => match values {
                Dictionary::Scaled { .. } => Shape::Num,
                Dictionary::Bool { .. } => Shape::Bool,
                Dictionary::Text { .. } => Shape::Text,
                _ => Shape::Dyn,
            },
            Values::List { .. } | Values::Struct { .. } | Values::Any(_) => Shape::Dyn,
        }
    }

    fn shape(index: usize, column: &zen_expression::lane::Column, dated: &[usize]) -> Shape {
        match dated.contains(&index) {
            true => Shape::Dyn,
            false => Self::tag(column),
        }
    }

    fn layout(columns: &Columns, dated: &[usize]) -> Option<Layout> {
        columns
            .columns
            .iter()
            .enumerate()
            .map(|(index, (path, column))| {
                (!path.is_empty()).then(|| (Arc::from(*path), Self::shape(index, column, dated)))
            })
            .collect::<Option<Vec<_>>>()
            .map(Layout)
    }

    fn matches(layout: &Layout, columns: &Columns, dated: &[usize]) -> bool {
        layout.0.len() == columns.columns.len()
            && layout.0.iter().zip(&columns.columns).enumerate().all(
                |(index, ((path, tag), (name, column)))| {
                    path.as_ref() == *name && *tag == Self::shape(index, column, dated)
                },
            )
    }

    pub fn root(
        &self,
        graph: &CompiledGraph,
        columns: &Columns,
        subgraphs: Option<&Arc<Subgraphs>>,
        dated: &[usize],
    ) -> Option<(Arc<Layout>, Bound)> {
        let closure = subgraphs.map(|subgraphs| subgraphs.closure(graph));
        if let Ok(cache) = self.roots.read() {
            if let Some(root) = cache.iter().find(|root| {
                Self::matches(&root.layout, columns, dated)
                    && closure
                        .as_ref()
                        .is_none_or(|closure| Subgraphs::same(closure, &root.closure))
            }) {
                return Some((root.layout.clone(), root.bound.clone()));
            }
        }
        let layout = Arc::new(Self::layout(columns, dated)?);
        let nested = Nested {
            subgraphs: subgraphs.cloned(),
            stack: Arc::from([]),
        };
        let bound = self.build(graph, layout.clone(), closure.unwrap_or_default(), nested);
        Some((layout, bound))
    }

    fn nested(
        &self,
        graph: &CompiledGraph,
        layout: Layout,
        subgraphs: &Arc<Subgraphs>,
        stack: Arc<[Arc<str>]>,
    ) -> (Arc<Layout>, Bound) {
        let closure = subgraphs.closure(graph);
        if let Ok(cache) = self.roots.read() {
            if let Some(root) = cache
                .iter()
                .find(|root| *root.layout == layout && Subgraphs::same(&closure, &root.closure))
            {
                return (root.layout.clone(), root.bound.clone());
            }
        }
        let layout = Arc::new(layout);
        let nested = Nested {
            subgraphs: Some(subgraphs.clone()),
            stack,
        };
        let bound = self.build(graph, layout.clone(), closure, nested);
        (layout, bound)
    }

    fn build(
        &self,
        graph: &CompiledGraph,
        layout: Arc<Layout>,
        closure: Closure,
        nested: Nested,
    ) -> Bound {
        let builder = Builder {
            graph,
            masks: Masks::default(),
            catalog: Catalog::default(),
            views: Vec::new(),
            objects: Vec::new(),
            hosted: Vec::new(),
            null: None,
            nested,
        };
        let bound: Bound = Arc::new(builder.segment(graph.root.clone(), &layout).map(Arc::new));
        if let Ok(mut cache) = self.roots.write() {
            cache.retain(|root| root.layout != layout);
            if cache.len() >= 16 {
                cache.remove(0);
            }
            cache.push(Root {
                layout,
                bound: bound.clone(),
                closure,
            });
        }
        bound
    }
}
