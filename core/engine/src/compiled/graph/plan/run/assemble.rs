use crate::compiled::graph::plan::Plan;
use crate::nodes::NodeHandlerExtensions;
use crate::compiled::leaf::{Bits, ColumnBuilder, Leaf};
use crate::compiled::graph::{ColumnarOutput, CompiledGraph, Kind, OutputColumn};
use crate::model::GraphContent;
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use super::{Host, Run};
use super::store::{View, Origin, Store};

impl<'a> Run<'_, 'a> {
    fn assemble(store: Store<'a>) -> ColumnarOutput<'a> {
        let rows = store.rows;
        let mut store = store;
        let groups = std::mem::take(&mut store.groups);
        let finish = |leaf: &Leaf<'a>, presence: &Option<Rc<[u64]>>| match presence {
            None => leaf.clone(),
            Some(mask) => Leaf::masked(leaf.clone(), mask),
        };
        let columns: Vec<(Arc<str>, Leaf<'a>)> = match groups.as_slice() {
            [(None, leaves)] => leaves.iter().map(|(path, leaf, presence)| (path.clone(), finish(leaf, presence))).collect(),
            _ => {
                let mut owner: Vec<Option<(usize, usize)>> = vec![None; rows];
                let mut reach: Vec<Vec<u64>> = Vec::with_capacity(groups.len());
                for (g, (positions, _)) in groups.iter().enumerate() {
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    match positions {
                        Some(positions) => positions.iter().enumerate().for_each(|(i, &row)| {
                            owner[row] = Some((g, i));
                            Bits::set(&mut bits, row, true);
                        }),
                        None => (0..rows).for_each(|row| {
                            owner[row] = Some((g, row));
                            Bits::set(&mut bits, row, true);
                        }),
                    }
                    reach.push(bits);
                }
                let mut paths: Vec<Arc<str>> = Vec::new();
                for (_, leaves) in &groups {
                    for (path, _, _) in leaves {
                        if !paths.contains(path) {
                            paths.push(path.clone());
                        }
                    }
                }
                paths
                    .into_iter()
                    .map(|path| {
                        let sources: Vec<Origin<'_, 'a>> = groups
                            .iter()
                            .map(|(_, leaves)| leaves.iter().find(|(p, _, _)| *p == path).map(|(_, l, m)| (l, m)))
                            .collect();
                        if let Some(Some((first, _))) = sources.iter().find(|s| s.is_some()) {
                            if sources.iter().flatten().all(|(s, _)| s.same(first)) {
                                if sources.iter().all(|s| s.is_some_and(|(_, m)| m.is_none())) {
                                    return (path, (*first).clone());
                                }
                                let mut combined = vec![0u64; rows.div_ceil(64)];
                                for (source, bits) in sources.iter().zip(&reach) {
                                    match source {
                                        None => {}
                                        Some((_, Some(mask))) => combined.iter_mut().zip(bits.iter().zip(mask.iter())).for_each(|(c, (b, m))| *c |= b & m),
                                        Some((_, None)) => combined.iter_mut().zip(bits).for_each(|(c, b)| *c |= b),
                                    }
                                }
                                return (path, Leaf::masked((*first).clone(), &combined.into()));
                            }
                        }
                        let views: Vec<Option<View>> = sources
                            .iter()
                            .zip(&groups)
                            .map(|(source, (positions, _))| {
                                source.map(|(leaf, presence)| match (leaf.scatter_parts(), positions) {
                                    (Some((inner, at)), Some(positions)) if Rc::ptr_eq(at, positions) => {
                                        (inner.column(), true, presence.as_ref(), inner.coded_dict())
                                    }
                                    _ => (leaf.column(), false, presence.as_ref(), leaf.coded_dict()),
                                })
                            })
                            .collect();
                        let located = |row: usize| -> Option<(usize, &Column, usize)> {
                            let (g, i) = owner[row]?;
                            let (view, local, presence, _) = views[g].as_ref()?;
                            if presence.is_some_and(|p| !Bits::get(p, row)) {
                                return None;
                            }
                            let at = if *local { i } else { row };
                            view.valid(at).then_some((g, view, at))
                        };
                        let cell = |row: usize| located(row).map(|(_, view, at)| (view, at));
                        if let Some(array) = Self::coded_gather(rows, &views, &located) {
                            return (path, Leaf::typed(array));
                        }
                        let family = |column: &Column| match column.values {
                            Values::Scaled { .. } | Values::I64(_) | Values::Dict { values: Dictionary::Scaled { .. }, .. } => 0u8,
                            Values::Bool { .. } | Values::Dict { values: Dictionary::Bool { .. }, .. } => 1,
                            Values::Text { .. } | Values::Utf8 { .. } | Values::Dict { values: Dictionary::Text { .. }, .. } => 2,
                            _ => 3,
                        };
                        let families: Vec<u8> = views.iter().flatten().map(|(v, _, _, _)| family(v)).collect();
                        let common = families.first().copied().filter(|f| families.iter().all(|x| x == f));
                        if let Some(array) = Self::typed_gather(rows, common, &cell) {
                            return (path, Leaf::typed(array));
                        }
                        let mut builder = ColumnBuilder::with_capacity(rows);
                        for row in 0..rows {
                            match cell(row) {
                                Some((view, at)) => builder.push_cell(view, at),
                                None => builder.push_null(),
                            }
                        }
                        (path, Leaf::typed(builder.finish()))
                    })
                    .collect()
            }
        };
        ColumnarOutput {
            rows,
            columns: columns
                .into_iter()
                .map(|(path, leaf)| (path, OutputColumn(CompiledGraph::cleaned(&leaf))))
                .collect(),
            errors: std::mem::take(&mut store.errors),
            typed: store.counts.0,
            laned: store.counts.1,
            hosted: 0,
            exact: Vec::new(),
        }
    }
}

impl Plan {
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate<'a>(
        &self,
        graph: &CompiledGraph,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &'a Columns<'a>,
        failures: Vec<(usize, Rc<EvaluationError>)>,
    ) -> Option<ColumnarOutput<'a>> {
        if let Some(resolved) = &content.resolved_schemas {
            let touched = graph
                .steps
                .iter()
                .flatten()
                .any(|step| !matches!(step.kind, Kind::Input | Kind::Output) && resolved.contains_key(&step.node.id));
            if touched {
                return None;
            }
        }
        let (layout, root) = self.root(graph, columns)?;
        let root = root.as_ref().as_ref().ok()?;
        let mut store = Store::new(root, columns);
        for (row, error) in failures {
            if let Some(slot) = store.errors.get_mut(row) {
                if slot.is_none() {
                    store.failed += 1;
                }
                *slot = Some(error);
            }
        }
        let mut run = Run {
            graph,
            layout: &layout,
            runner: CompiledGraph::RUNNER.with_borrow_mut(std::mem::take),
            host: Host {
                content,
                extensions,
                max_depth,
            },
            store,
            settled: true,
            failures: Vec::new(),
            shreds: Vec::new(),
        };
        let done = run.walk(root.clone()).await;
        let Run { runner, store, .. } = run;
        CompiledGraph::RUNNER.with_borrow_mut(|slot| *slot = runner);
        done.ok()?;
        Some(Run::assemble(store))
    }
}
