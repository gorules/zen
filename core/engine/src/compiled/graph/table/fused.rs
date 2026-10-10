use crate::compiled::graph::bound::{Bound, Outs};
use crate::compiled::leaf::{Array, Bits, Leaf};
use std::sync::Arc;
use zen_expression::lane::{Binding, LaneProgram, LaneRunner, Op};
use super::{Column, Table};

pub(super) struct FusedCells {
    program: LaneProgram,
    cells: Vec<(usize, usize)>,
}

impl Table {
    pub(super) fn fused(columns: &[Column], plain: &[(usize, usize, Arc<str>)], lazy: bool) -> Option<FusedCells> {
        let mut entries: Vec<(Arc<str>, Option<Arc<str>>)> = Vec::new();
        let mut cells: Vec<(usize, usize)> = Vec::new();
        for (index, column) in columns.iter().enumerate() {
            match column {
                Column::Field { field, cells: set, .. } if !lazy && set.others() > 0 && !set.peelable() && field.path().is_some() => {
                    let Some(target) = field.source() else {
                        continue;
                    };
                    let sources: Option<Vec<Arc<str>>> = (0..set.others())
                        .map(|other| set.other(other).and_then(|(program, _)| program.source()).map(Arc::from))
                        .collect();
                    let Some(sources) = sources else {
                        continue;
                    };
                    for (other, source) in sources.into_iter().enumerate() {
                        entries.push((source, Some(Arc::from(target))));
                        cells.push((index, other));
                    }
                }
                Column::Plain { cells: programs, .. } if !lazy || plain.len() <= 8 || !programs.iter().flat_map(|(p, _)| p).any(Self::heavy) => {
                    for (_, cell, source) in plain.iter().filter(|(column, _, _)| *column == index) {
                        entries.push((source.clone(), None));
                        cells.push((index, *cell));
                    }
                }
                Column::Field { .. } | Column::Plain { .. } => {}
            }
        }
        if entries.is_empty() || entries.len() > Self::FUSED {
            return None;
        }
        let refs: Vec<(&str, Option<&str>)> = entries.iter().map(|(s, f)| (s.as_ref(), f.as_deref())).collect();
        LaneProgram::compile_cells(&refs).ok().map(|program| FusedCells { program, cells })
    }

    fn heavy(program: &LaneProgram) -> bool {
        program
            .program()
            .steps
            .iter()
            .any(|step| matches!(step.op, Op::Call { .. } | Op::Method { .. } | Op::Closure(_) | Op::LoadCall(_)))
    }

    pub(super) fn fused_truths(&self, runner: &mut LaneRunner, bound: &Bound) -> Option<Vec<Vec<u64>>> {
        let fused = self.fused.as_ref()?;
        let p = fused.program.program();
        let bound_only = p
            .site_keys
            .iter()
            .all(|key| key.as_deref().is_some_and(|key| !matches!((bound.bind)(key), Binding::Row)));
        if p.opaque() || !bound_only {
            return None;
        }
        let rows = bound.rows();
        let mut outs = Outs::take();
        let mut failed: Vec<(usize, usize)> = Vec::new();
        bound.export(runner, &fused.program, None, &mut outs, |row, stage, _| failed.push((row, stage)));
        let mut truths: Vec<Vec<u64>> = outs
            .iter_mut()
            .map(|out| Leaf::typed(Array::from_output(out)).truths(rows))
            .collect();
        truths.resize(fused.cells.len(), vec![0u64; rows.div_ceil(64)]);
        for (row, stage) in failed {
            if let Some(truth) = truths.get_mut(stage) {
                Bits::set(truth, row, false);
            }
        }
        Some(truths)
    }

    pub(super) fn fused_at(&self, column: usize, cell: usize) -> Option<usize> {
        self.fused.as_ref()?.cells.iter().position(|c| *c == (column, cell))
    }

    pub(super) fn fuses(&self, column: usize) -> bool {
        self.fused.as_ref().is_some_and(|fused| fused.cells.iter().any(|(c, _)| *c == column))
    }
}
