use super::store::{Group, Origin, Store, View};
use super::{Host, Run};
use crate::compiled::graph::columnar::Admitted;
use crate::compiled::graph::plan::subgraphs::Subgraphs;
use crate::compiled::graph::plan::Plan;
use crate::compiled::graph::{ColumnarOutput, CompiledGraph, Completed, Kind};
use crate::compiled::leaf::{Bits, ColumnBuilder, Leaf};
use crate::compiled::output::OutputColumn;
use crate::model::GraphContent;
use crate::nodes::NodeHandlerExtensions;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Dictionary, Values};

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
            [Group {
                positions: None,
                leaves,
            }] => leaves
                .iter()
                .map(|(path, leaf, presence)| (path.clone(), finish(leaf, presence)))
                .collect(),
            _ => {
                let mut owner: Vec<Option<(usize, usize)>> = vec![None; rows];
                let mut reach: Vec<Vec<u64>> = Vec::with_capacity(groups.len());
                for (g, Group { positions, .. }) in groups.iter().enumerate() {
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
                for Group { leaves, .. } in &groups {
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
                            .map(|Group { leaves, .. }| {
                                leaves
                                    .iter()
                                    .find(|(p, _, _)| *p == path)
                                    .map(|(_, l, m)| (l, m))
                            })
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
                                        Some((_, Some(mask))) => combined
                                            .iter_mut()
                                            .zip(bits.iter().zip(mask.iter()))
                                            .for_each(|(c, (b, m))| *c |= b & m),
                                        Some((_, None)) => {
                                            combined.iter_mut().zip(bits).for_each(|(c, b)| *c |= b)
                                        }
                                    }
                                }
                                return (path, Leaf::masked((*first).clone(), &combined.into()));
                            }
                        }
                        let views: Vec<Option<View>> = sources
                            .iter()
                            .zip(&groups)
                            .map(|(source, Group { positions, .. })| {
                                source.map(|(leaf, presence)| {
                                    match (leaf.scatter_parts(), positions) {
                                        (Some((inner, at)), Some(positions))
                                            if Rc::ptr_eq(at, positions) =>
                                        {
                                            View {
                                                column: inner.column(),
                                                local: true,
                                                presence: presence.as_ref(),
                                                dict: inner.coded_dict(),
                                            }
                                        }
                                        _ => View {
                                            column: leaf.column(),
                                            local: false,
                                            presence: presence.as_ref(),
                                            dict: leaf.coded_dict(),
                                        },
                                    }
                                })
                            })
                            .collect();
                        let located = |row: usize| -> Option<(usize, &Column, usize)> {
                            let (g, i) = owner[row]?;
                            let view = views[g].as_ref()?;
                            if view.presence.is_some_and(|p| !Bits::get(p, row)) {
                                return None;
                            }
                            let at = if view.local { i } else { row };
                            view.column.valid(at).then_some((g, &view.column, at))
                        };
                        let cell = |row: usize| located(row).map(|(_, view, at)| (view, at));
                        if let Some(array) = Self::coded_gather(rows, &views, &located) {
                            return (path, Leaf::typed(array));
                        }
                        let family = |column: &Column| match column.values {
                            Values::Scaled { .. }
                            | Values::I64(_)
                            | Values::Dict {
                                values: Dictionary::Scaled { .. },
                                ..
                            } => 0u8,
                            Values::Bool { .. }
                            | Values::Dict {
                                values: Dictionary::Bool { .. },
                                ..
                            } => 1,
                            Values::Text { .. }
                            | Values::Utf8 { .. }
                            | Values::Dict {
                                values: Dictionary::Text { .. },
                                ..
                            } => 2,
                            _ => 3,
                        };
                        let families: Vec<u8> = views
                            .iter()
                            .flatten()
                            .map(|view| family(&view.column))
                            .collect();
                        let common = families
                            .first()
                            .copied()
                            .filter(|f| families.iter().all(|x| x == f));
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
            errors: store.errors.take(),
            paths: crate::diagnostics::Paths {
                typed: store.counts.0,
                laned: store.counts.1,
                hosted: 0,
            },
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
        admitted: Admitted<'a>,
    ) -> PlanOutcome<'a> {
        if let Some(resolved) = &content.resolved_schemas {
            let touched = graph.steps.iter().flatten().any(|step| {
                !matches!(step.kind, Kind::Input | Kind::Output)
                    && resolved.contains_key(&step.node.id)
            });
            if touched {
                return PlanOutcome::Skipped;
            }
        }
        let Admitted { failures, dated } = admitted;
        let indexes: Vec<usize> = dated.iter().map(|(index, _)| *index).collect();
        let subgraphs = match Subgraphs::wanted(graph) {
            true => Some(Arc::new(Subgraphs::resolve(graph, extensions).await)),
            false => None,
        };
        let Some((layout, root)) = self.root(graph, columns, subgraphs.as_ref(), &indexes) else {
            return PlanOutcome::Skipped;
        };
        let Ok(root) = root.as_ref().as_ref() else {
            return PlanOutcome::Skipped;
        };
        let mut store = Store::new(root, columns, dated);
        for (row, error) in failures {
            store.errors.set(row, error);
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
            completed: Completed::default(),
            iteration: 0,
            parents: None,
        };
        run.walk(root.clone()).await;
        let Run {
            runner,
            store,
            completed,
            ..
        } = run;
        CompiledGraph::RUNNER.with_borrow_mut(|slot| *slot = runner);
        let rows = store.errors.deferred();
        let output = Run::assemble(store);
        match rows.is_empty() {
            true => PlanOutcome::Done(output),
            false => PlanOutcome::Partial {
                output,
                rows,
                completed,
            },
        }
    }
}

pub(crate) enum PlanOutcome<'a> {
    Skipped,
    Done(ColumnarOutput<'a>),
    Partial {
        output: ColumnarOutput<'a>,
        rows: Vec<usize>,
        completed: Completed,
    },
}
