use super::store::{Layered, Store};
use super::Run;
use crate::compiled::census::Census;
use crate::compiled::graph::call::Call;
use crate::compiled::graph::data::Data;
use crate::compiled::graph::plan::view::Shape;
use crate::compiled::graph::plan::{ChildOp, Plan, SegPlan};
use crate::compiled::graph::{CompiledGraph, Completed, Host, Kind};
use crate::compiled::host::{NodeError, NodeHandlerExtensions};
use crate::compiled::leaf::{Array, Bits, Leaf};
use crate::model::DecisionNodeKind;
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;

struct Produced<'a> {
    leaves: Vec<(Leaf<'a>, Rc<[u64]>)>,
    failed: Vec<(usize, NodeError)>,
}

impl CompiledGraph {
    pub(in crate::compiled::graph) fn input_name(&self) -> Option<Arc<str>> {
        self.steps
            .iter()
            .flatten()
            .find(|step| matches!(step.kind, Kind::Input))
            .map(|step| step.node.name.clone())
    }
}

impl<'a> Run<'_, 'a> {
    pub(super) async fn child(
        &mut self,
        seg: &SegPlan,
        op: &ChildOp,
        positions: Option<&Rc<[usize]>>,
    ) {
        let graph = self.graph;
        let Some(step) = graph.step_at(op.node) else {
            return;
        };
        let DecisionNodeKind::DecisionNode { content } = &step.node.kind else {
            return;
        };
        let at: Rc<[usize]> = positions
            .cloned()
            .unwrap_or_else(|| (0..self.store.rows).collect());
        let count = at.len();
        let produced = match self.iteration.saturating_add(1) >= self.host.max_depth {
            true => Produced {
                leaves: Vec::new(),
                failed: (0..count)
                    .map(|i| {
                        (
                            i,
                            NodeError::new(
                                step.node.id.clone(),
                                EvaluationError::DepthLimitExceeded.to_string(),
                            ),
                        )
                    })
                    .collect(),
            },
            false => match self.columns(seg, op, &at) {
                Some(leaves) => {
                    Census::mark("columnar");
                    self.nested(op, &step.node.id, content, &at, leaves).await
                }
                None => {
                    Census::mark("batched");
                    self.fallback(seg, op, content, positions, &at).await
                }
            },
        };
        let mut failed = vec![false; count];
        for (i, error) in produced.failed {
            failed[i] = true;
            self.store.errors.fault(at[i], error);
        }
        let mut placed: Vec<(Leaf<'a>, Rc<[u64]>)> = Vec::with_capacity(op.keys.len());
        for (index, key) in op.keys.iter().enumerate() {
            let (leaf, bits) = match produced.leaves.get(index) {
                Some((leaf, bits)) => {
                    let mut bits = bits.to_vec();
                    for (i, &fail) in failed.iter().enumerate() {
                        if fail {
                            Bits::set(&mut bits, i, false);
                        }
                    }
                    (leaf.clone(), Rc::<[u64]>::from(bits))
                }
                None => (Leaf::nulls(count), Rc::from(vec![0u64; count.div_ceil(64)])),
            };
            let spread = self.store.spread(&bits, positions);
            let leaf_at = self.store.place(leaf.clone(), positions);
            self.store.slots[key.out as usize] = Some(leaf_at);
            self.store.produce(key.present, spread);
            placed.push((leaf, bits));
        }
        if op.record {
            self.record(seg, op, &step.node.id, content, &at, &placed, &failed);
        }
    }

    fn columns(&mut self, seg: &SegPlan, op: &ChildOp, at: &Rc<[usize]>) -> Option<Vec<Leaf<'a>>> {
        let mut layered: Vec<Layered<'a>> = Vec::new();
        self.layer(seg, &op.view, "", &mut layered)?;
        if layered.len() != op.layout.0.len() {
            return None;
        }
        let count = at.len();
        let full = count == self.store.rows;
        let mut leaves = Vec::with_capacity(layered.len());
        for (path, shape) in &op.layout.0 {
            let (_, leaf, bits) = layered.iter().find(|(p, _, _)| p == path)?;
            let (leaf, bits): (Leaf<'a>, Rc<[u64]>) = match full {
                true => (leaf.clone(), bits.clone()),
                false => (
                    leaf.pick(at),
                    Bits::of(count, |i| Bits::get(bits, at[i])).into(),
                ),
            };
            let leaf = match Bits::full(&bits, 0, count) {
                true => leaf,
                false => Leaf::masked(leaf, &bits),
            };
            let tag = Plan::tag(&leaf.column());
            if matches!(shape, Shape::Num | Shape::Bool | Shape::Text) && tag != *shape {
                return None;
            }
            leaves.push(leaf);
        }
        Some(leaves)
    }

    async fn nested(
        &mut self,
        op: &ChildOp,
        id: &Arc<str>,
        content: &zen_types::decision::DecisionNodeContent,
        at: &Rc<[usize]>,
        leaves: Vec<Leaf<'a>>,
    ) -> Produced<'a> {
        let count = at.len();
        let Ok(graph) = Call::compiled(&op.content) else {
            return Produced {
                leaves: Vec::new(),
                failed: Vec::new(),
            };
        };
        let columns = Columns {
            rows: count,
            columns: op
                .layout
                .0
                .iter()
                .zip(&leaves)
                .map(|((path, _), leaf)| (path.as_ref(), leaf.column()))
                .collect::<Vec<(&str, Column)>>(),
        };
        let call = Call {
            key: &content.key,
            extensions: self.host.extensions,
            max_depth: self.host.max_depth,
            iteration: self.iteration,
        };
        let extensions = NodeHandlerExtensions {
            function_runtime: Default::default(),
            ..call.sub_extensions(&op.content)
        };
        let named: Vec<(&Arc<str>, Symbol, Leaf<'a>)> = op
            .nodes
            .iter()
            .flatten()
            .map(|(name, r)| (name, Symbol::from(name.as_ref()), self.store.leaf(r.slot)))
            .collect();
        let outer = self.parents;
        let entry = outer.and_then(|_| self.graph.input_name());
        let parents = |row: usize| -> Option<Variable> {
            op.nodes.as_ref()?;
            let source = *at.get(row)?;
            let mut map = VariableMap::with_capacity(named.len());
            for (original, name, leaf) in &named {
                let value = leaf.get(source);
                let value = match (&entry, outer) {
                    (Some(entry), Some(outer)) if *original == entry => match outer(source) {
                        Some(parent) => {
                            let view = value.depth_clone(1);
                            view.dot_insert(Variable::nodes_key().as_ref(), parent);
                            view
                        }
                        None => value,
                    },
                    _ => value,
                };
                map.insert(name.clone(), value);
            }
            Some(Variable::from_object(map))
        };
        let store = Store::new(&op.root, &columns, Vec::new());
        let mut run = Run {
            graph,
            layout: &op.layout,
            runner: std::mem::take(&mut self.runner),
            host: Host {
                content: &op.content,
                extensions: &extensions,
                max_depth: self.host.max_depth,
            },
            store,
            settled: true,
            failures: Vec::new(),
            shreds: Vec::new(),
            completed: Completed::default(),
            iteration: self.iteration + 1,
            parents: Some(&parents),
        };
        Box::pin(run.walk(op.root.clone())).await;
        let Run {
            runner, mut store, ..
        } = run;
        self.runner = runner;
        let deferred = store.errors.deferred();
        for &row in &deferred {
            self.store.errors.defer(at[row]);
        }
        let failed: Vec<(usize, NodeError)> = (0..count)
            .filter(|&row| !deferred.contains(&row))
            .filter_map(|row| {
                let error = store.errors.get(row)?;
                Some((row, NodeError::new(id.clone(), error.to_string())))
            })
            .collect();
        let mut produced = Vec::with_capacity(op.keys.len());
        for key in &op.keys {
            let leaf = store.leaf(key.child.slot);
            let bits = store.mask(&op.root, key.child.present);
            produced.push((Self::detach(&leaf, count), bits));
        }
        Produced {
            leaves: produced,
            failed,
        }
    }

    fn detach<'x>(leaf: &Leaf<'_>, rows: usize) -> Leaf<'x> {
        match leaf {
            Leaf::Typed(array) => Leaf::Typed(array.clone()),
            Leaf::Any(values) => Leaf::Any(values.clone()),
            Leaf::Templated(templated) => Leaf::Templated(templated.clone()),
            other => {
                let positions: Vec<usize> = (0..rows).collect();
                Leaf::typed(Array::gather(&other.column(), &positions))
            }
        }
    }

    async fn fallback(
        &mut self,
        seg: &SegPlan,
        op: &ChildOp,
        content: &zen_types::decision::DecisionNodeContent,
        positions: Option<&Rc<[usize]>>,
        at: &Rc<[usize]>,
    ) -> Produced<'a> {
        let count = at.len();
        let (_, inputs, nodes) = self.inputs(seg, Some(&op.view), &op.nodes, positions);
        let step = self.graph.step_at(op.node);
        let Some(step) = step else {
            return Produced {
                leaves: Vec::new(),
                failed: Vec::new(),
            };
        };
        let results = CompiledGraph::nested_results(
            step,
            content,
            &op.content,
            &inputs,
            &nodes,
            self.host.extensions,
            self.host.max_depth,
            self.iteration,
        )
        .await;
        let mut failed = Vec::new();
        let values: Vec<Option<Variable>> = results
            .into_iter()
            .enumerate()
            .map(|(i, result)| match result {
                Ok(value) => Some(value),
                Err(error) => {
                    failed.push((i, error));
                    None
                }
            })
            .collect();
        let leaves = op
            .keys
            .iter()
            .map(|key| {
                let mut bits = vec![0u64; count.div_ceil(64)];
                let column: Vec<Variable> = values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| {
                        let found = value.as_ref().and_then(|value| match key.path.is_empty() {
                            true => Some(value.clone()),
                            false => Data::lookup(value, &key.path),
                        });
                        match found {
                            Some(found) => {
                                Bits::set(&mut bits, i, true);
                                found
                            }
                            None => Variable::Null,
                        }
                    })
                    .collect();
                (Leaf::Any(column.into()), Rc::<[u64]>::from(bits))
            })
            .collect();
        Produced { leaves, failed }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        seg: &SegPlan,
        op: &ChildOp,
        id: &Arc<str>,
        content: &zen_types::decision::DecisionNodeContent,
        at: &Rc<[usize]>,
        placed: &[(Leaf<'a>, Rc<[u64]>)],
        failed: &[bool],
    ) {
        let attributes = &content.transform_attributes;
        let input = attributes
            .pass_through
            .then(|| self.materialize(seg, &op.input));
        let results: Vec<Result<Variable, NodeError>> = (0..at.len())
            .map(|i| {
                if failed[i] {
                    return Err(NodeError::new(id.clone(), String::new()));
                }
                let value = match op.keys.first() {
                    Some(key) if key.path.is_empty() => placed
                        .first()
                        .map_or(Variable::Null, |(leaf, _)| leaf.get(i)),
                    _ => {
                        let object = Variable::empty_object();
                        for (key, (leaf, bits)) in op.keys.iter().zip(placed) {
                            if !Bits::get(bits, i) {
                                continue;
                            }
                            let value = leaf.get(i);
                            if !matches!(value, Variable::Null) {
                                object.dot_insert(&key.path, value);
                            }
                        }
                        object
                    }
                };
                let source = input
                    .as_ref()
                    .map_or(Variable::Null, |leaf| leaf.get(at[i]));
                Ok(crate::compiled::exact::Attributes::finish(
                    attributes.output_path.as_deref(),
                    attributes.pass_through,
                    &source,
                    value,
                ))
            })
            .collect();
        self.completed
            .record(op.node, self.store.rows, at, &results);
    }

    pub(super) fn measured(&self, positions: Option<&Rc<[usize]>>) {
        let rows = positions.map_or(self.store.rows, |p| p.len());
        Census::record(
            format!("decision | child | {}", Census::path()),
            rows,
            Census::elapsed(),
        );
    }
}
