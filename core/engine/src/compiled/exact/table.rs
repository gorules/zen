use crate::compiled::exact::graph::{Ctx, Row};
use crate::decision_graph::tracer::{DecisionTableNodeTrace, DecisionTableRowTrace};
use crate::nodes::decision_table::index::TableIndex;
use crate::nodes::{NodeResponse, NodeResult};
use crate::policy::blocks::{DecisionTableIr, ExecutionError, TableSelection};
use crate::policy::host::Site;
use crate::workspace::types::{BlockTrace, DecisionTableExtras};
use ahash::{HashMap, HashSet};
use base64::Engine as _;
use fixedbitset::FixedBitSet;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::Session;
use zen_expression::IsolateError;
use zen_types::decision::{
    DecisionTableContent, DecisionTableHitPolicy, DecisionTableInputField, DecisionTableOutputField,
};
use zen_types::variable::Variable;

pub(crate) type Rule = HashMap<Arc<str>, Arc<str>>;
type Cells = (Option<Variable>, Vec<(usize, Variable)>);

pub(crate) struct RowTable<'t, 's> {
    pub session: &'t mut Session,
    pub inputs: &'t [DecisionTableInputField],
    index: Option<&'t TableIndex>,
    site: Option<&'t Site<'s>>,
    refs: Vec<Option<Variable>>,
}

impl<'t, 's> RowTable<'t, 's> {
    pub fn new(
        session: &'t mut Session,
        inputs: &'t [DecisionTableInputField],
        index: Option<&'t TableIndex>,
        site: Option<&'t Site<'s>>,
    ) -> Self {
        Self {
            session,
            inputs,
            index,
            site,
            refs: vec![None; inputs.len()],
        }
    }

    fn fail<T>(
        &self,
        source: &Arc<str>,
        error: IsolateError,
        lenient: T,
    ) -> Result<T, ExecutionError> {
        match self.site {
            Some(site) => Err(site.expression_error(source, error)),
            None => Ok(lenient),
        }
    }

    fn field(&self, col: usize) -> Option<&'t Arc<str>> {
        self.inputs
            .get(col)?
            .field
            .as_ref()
            .filter(|f| !f.is_empty())
    }

    pub fn reference(&mut self, col: usize) -> Result<Option<Variable>, ExecutionError> {
        if let Some(value) = &self.refs[col] {
            return Ok(Some(value.shallow_clone()));
        }
        let Some(field) = self.field(col) else {
            return Ok(None);
        };
        match self.session.run_standard(field) {
            Ok(value) => {
                self.refs[col] = Some(value.shallow_clone());
                Ok(Some(value))
            }
            Err(error) => self.fail(field, error, None),
        }
    }

    fn bind(&mut self, col: usize) -> Result<bool, ExecutionError> {
        let Some(value) = self.reference(col)? else {
            return Ok(false);
        };
        match self.session.set_reference_value(value) {
            Ok(()) => Ok(true),
            Err(error) => self.fail(
                self.field(col).unwrap_or(&self.inputs[col].id),
                error,
                false,
            ),
        }
    }

    pub fn candidates(&mut self) -> Result<Option<FixedBitSet>, ExecutionError> {
        let Some(index) = self.index else {
            return Ok(None);
        };
        let mut acc: Option<FixedBitSet> = None;
        for (col, column) in index.columns.iter().enumerate() {
            let Some(column) = column else {
                continue;
            };
            if self.field(col).is_none() {
                continue;
            }
            let Some(value) = self.reference(col)? else {
                return Ok(None);
            };
            if self.site.is_none() {
                let _ = self.session.set_reference_value(value.shallow_clone());
            }
            if matches!(value, Variable::Dynamic(_)) {
                return Ok(None);
            }
            let hit = column.rows_for(&value);
            match &mut acc {
                None => {
                    let mut first = column.fallback.clone();
                    if let Some(hit) = hit {
                        first.union_with(hit);
                    }
                    acc = Some(first);
                }
                Some(acc) => {
                    let fallback = column.fallback.as_slice();
                    let hit = hit.map(FixedBitSet::as_slice).unwrap_or_default();
                    for (i, word) in acc.as_mut_slice().iter_mut().enumerate() {
                        *word &= fallback.get(i).copied().unwrap_or(0)
                            | hit.get(i).copied().unwrap_or(0);
                    }
                }
            }
        }
        Ok(acc)
    }

    pub fn cell(&mut self, col: usize, rule: &Rule) -> Result<bool, ExecutionError> {
        let Some(cell) = rule.get(&self.inputs[col].id).filter(|c| !c.is_empty()) else {
            return Ok(true);
        };
        if self.field(col).is_none() {
            return match self.session.run_standard(cell) {
                Ok(result) => Ok(result.as_bool().unwrap_or(false)),
                Err(error) => self.fail(cell, error, false),
            };
        }
        if !self.bind(col)? {
            return Ok(false);
        }
        match self.session.run_unary(cell) {
            Ok(pass) => Ok(pass),
            Err(error) => self.fail(cell, error, false),
        }
    }

    pub fn matches(
        &mut self,
        row: usize,
        rule: &Rule,
        pruned: bool,
    ) -> Result<bool, ExecutionError> {
        for col in 0..self.inputs.len() {
            if pruned && self.index.is_some_and(|index| index.decides(col, row)) {
                if self.site.is_none()
                    && rule
                        .get(&self.inputs[col].id)
                        .is_some_and(|c| !c.is_empty())
                {
                    self.bind(col)?;
                }
                continue;
            }
            if !self.cell(col, rule)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn each(&mut self, rule: &Rule, unreached: bool) -> Result<Vec<bool>, ExecutionError> {
        let mut failed = false;
        (0..self.inputs.len())
            .map(|col| {
                let passed = match self.cell(col, rule) {
                    Ok(passed) => passed,
                    Err(_) if unreached || failed => false,
                    Err(e) => return Err(e),
                };
                failed |= !passed;
                Ok(passed)
            })
            .collect()
    }

    pub fn output(&mut self, cell: &Arc<str>) -> Result<Option<Variable>, ExecutionError> {
        match self.session.run_standard(cell) {
            Ok(value) => Ok(Some(value)),
            Err(error) => self.fail(cell, error, None),
        }
    }
}

impl RowTable<'_, '_> {
    fn hits(
        &mut self,
        rules: &[Rule],
        candidates: &Option<FixedBitSet>,
        from: usize,
    ) -> Result<Option<usize>, ExecutionError> {
        for (row, rule) in rules.iter().enumerate().skip(from) {
            if candidates.as_ref().is_some_and(|c| !c.contains(row)) {
                continue;
            }
            if self.matches(row, rule, candidates.is_some())? {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn row(
        &mut self,
        outputs: &[DecisionTableOutputField],
        rule: &Rule,
        scalars: bool,
        split: bool,
    ) -> Option<Cells> {
        let object = scalars.then(Variable::empty_object);
        let mut lists = Vec::new();
        for (column, output) in outputs.iter().enumerate() {
            let (path, collect) = output.write_path();
            let collect = collect && split;
            if path.is_empty() || (!collect && !scalars) {
                continue;
            }
            let Some(cell) = rule.get(&output.id).filter(|c| !c.is_empty()) else {
                continue;
            };
            let value = self.output(cell).ok()??.deep_clone();
            match (collect, &object) {
                (true, _) => lists.push((column, value)),
                (false, Some(object)) => {
                    object.dot_insert(path, value);
                }
                (false, None) => {}
            }
        }
        Some((object, lists))
    }

    fn trace(&mut self, index: usize, rule: &Rule) -> DecisionTableRowTrace {
        let mut expressions: HashMap<Rc<str>, Rc<str>> = HashMap::default();
        let mut reference_map: HashMap<Rc<str>, Variable> = HashMap::default();
        expressions.insert(
            Rc::from("_id"),
            rule.get("_id")
                .map_or_else(|| Rc::from(""), |id| Rc::from(id.as_ref())),
        );
        if let Some(description) = rule.get("_description") {
            expressions.insert(Rc::from("_description"), Rc::from(description.as_ref()));
        }
        for (col, input) in self.inputs.iter().enumerate() {
            let (Some(cell), Some(field)) = (rule.get(&input.id), &input.field) else {
                continue;
            };
            if let Ok(Some(reference)) = self.reference(col) {
                reference_map.insert(Rc::from(field.as_ref()), reference);
            }
            expressions.insert(
                Rc::from(format!("{field}[{}]", input.id).as_str()),
                Rc::from(cell.as_ref()),
            );
        }
        DecisionTableRowTrace {
            index,
            reference_map,
            rule: expressions,
        }
    }

    pub(crate) fn graph(
        row: Row,
        content: &DecisionTableContent,
        index: Option<&TableIndex>,
    ) -> NodeResult {
        let mut ctx = Ctx::<DecisionTableNodeTrace>::new(row);
        let mut session = ctx.row.session();
        let traced = ctx.row.trace;
        let mut table = RowTable::new(
            &mut session,
            &content.inputs,
            index.filter(|_| !traced),
            None,
        );
        let candidates = table.candidates().ok().flatten();
        let first = matches!(content.hit_policy, DecisionTableHitPolicy::First);
        let split = first && content.outputs.iter().any(|output| output.write_path().1);
        let mut scalars: Option<Variable> = None;
        let mut items = Vec::new();
        let mut lists: Vec<Vec<Variable>> = vec![Vec::new(); content.outputs.len()];
        let mut traces = Vec::new();
        let slot = |column: usize| {
            let path = content.outputs[column].write_path().0;
            content
                .outputs
                .iter()
                .position(|o| o.write_path() == (path, true))
                .unwrap_or(column)
        };
        let mut from = 0;
        while let Ok(Some(at)) = table.hits(&content.rules, &candidates, from) {
            from = at + 1;
            let rule = &content.rules[at];
            let Some((object, values)) =
                table.row(&content.outputs, rule, scalars.is_none() || !split, split)
            else {
                continue;
            };
            if traced {
                traces.push(table.trace(at, rule));
            }
            match (first, split, object) {
                (true, false, Some(object)) => {
                    ctx.trace(|t| {
                        *t = traces.pop().map_or_else(
                            DecisionTableNodeTrace::default,
                            DecisionTableNodeTrace::FirstHit,
                        )
                    });
                    return ctx.success(object);
                }
                (false, _, Some(object)) => items.push(object),
                (_, _, object) => {
                    scalars = scalars
                        .or(object)
                        .or_else(|| Some(Variable::empty_object()));
                    values
                        .into_iter()
                        .for_each(|(column, value)| lists[slot(column)].push(value));
                }
            }
        }
        if first && !split {
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }
        ctx.trace(|t| *t = DecisionTableNodeTrace::Collect(traces));
        let output = match split {
            false => Variable::from_array(items),
            true => {
                let output = scalars.unwrap_or_else(Variable::empty_object);
                for (column, output_field) in content.outputs.iter().enumerate() {
                    let (path, collect) = output_field.write_path();
                    if collect && !path.is_empty() && slot(column) == column {
                        output.dot_insert(
                            path,
                            Variable::from_array(std::mem::take(&mut lists[column])),
                        );
                    }
                }
                output
            }
        };
        ctx.success(output)
    }

    pub(crate) fn select(
        &mut self,
        ir: &DecisionTableIr,
        extras: bool,
    ) -> Result<TableSelection, ExecutionError> {
        let active: Vec<_> = ir.outputs.iter().filter(|c| !c.field.is_empty()).collect();
        let has_collect = active.iter().any(|c| c.collect);
        let mut pending = active.iter().filter(|c| !c.collect).count();
        let mut taken: Vec<bool> = vec![false; active.len()];
        let mut matched_rows: Vec<u32> = Vec::new();
        let mut used_cells: Vec<(u32, Arc<str>)> = Vec::new();
        let width = ir.inputs.len().div_ceil(8);
        let mut input_bits: Option<Vec<u8>> = extras.then(|| vec![0u8; ir.rules.len() * width]);
        let candidates = match input_bits {
            None => self.candidates()?,
            Some(_) => None,
        };
        for (row, rule) in ir.rules.iter().enumerate() {
            let satisfied = !has_collect && pending == 0 && !matched_rows.is_empty();
            if satisfied && !extras {
                break;
            }
            let hit = match input_bits.as_mut() {
                Some(bits) => {
                    let each = self.each(rule, satisfied)?;
                    each.iter()
                        .enumerate()
                        .filter(|(_, p)| **p)
                        .for_each(|(col, _)| bits[row * width + col / 8] |= 1 << (col % 8));
                    each.iter().all(|p| *p)
                }
                None => {
                    !candidates.as_ref().is_some_and(|c| !c.contains(row))
                        && self.matches(row, rule, candidates.is_some())?
                }
            };
            if !hit || satisfied {
                continue;
            }
            matched_rows.push(row as u32);
            for (at, col) in active.iter().enumerate() {
                if rule.get(&col.id).filter(|c| !c.is_empty()).is_none()
                    || (!col.collect && taken[at])
                {
                    continue;
                }
                if !col.collect {
                    taken[at] = true;
                    pending -= 1;
                }
                used_cells.push((row as u32, col.id.clone()));
            }
        }
        Ok(TableSelection {
            matched_rows,
            used_cells,
            input_bits,
        })
    }

    pub(crate) fn commit(
        &mut self,
        ir: &DecisionTableIr,
        selection: &TableSelection,
        site: &Site,
    ) -> Result<BlockTrace, ExecutionError> {
        let used: HashSet<(u32, &str)> = selection
            .used_cells
            .iter()
            .map(|(row, col)| (*row, col.as_ref()))
            .collect();
        let mut collected: HashMap<Arc<str>, Vec<Variable>> = HashMap::default();
        let mut written: HashSet<Arc<str>> = HashSet::default();
        for col in ir
            .outputs
            .iter()
            .filter(|col| col.collect && !col.field.is_empty())
        {
            collected.entry(col.field.clone()).or_default();
        }
        let mut evaluations: Vec<HashMap<Arc<str>, Variable>> = Vec::new();
        for &row in &selection.matched_rows {
            let Some(rule) = ir.rules.get(row as usize) else {
                continue;
            };
            let mut outputs: HashMap<Arc<str>, Variable> = HashMap::default();
            for col in &ir.outputs {
                let Some(cell) = rule
                    .get(&col.id)
                    .filter(|c| !c.is_empty() && used.contains(&(row, col.id.as_ref())))
                else {
                    continue;
                };
                let value = self.output(cell)?.unwrap_or(Variable::Null);
                if site.trace {
                    outputs.insert(col.field.clone(), value.deep_clone());
                }
                match col.collect {
                    true => collected.entry(col.field.clone()).or_default().push(value),
                    false => {
                        site.write(&col.field, value);
                        written.insert(col.field.clone());
                    }
                }
            }
            if site.trace {
                evaluations.push(outputs);
            }
        }
        for col in ir
            .outputs
            .iter()
            .filter(|col| !col.collect && !col.field.is_empty())
        {
            if written.insert(col.field.clone()) {
                site.write(&col.field, Variable::Null);
            }
        }
        for col in &ir.outputs {
            if let Some(values) = collected.remove(&col.field) {
                site.write(&col.field, Variable::from_array(values));
            }
        }
        Ok(BlockTrace::DecisionTable {
            matched_rows: selection.matched_rows.clone(),
            evaluations,
            extras: selection
                .input_bits
                .as_ref()
                .map(|bits| DecisionTableExtras {
                    input_pass: base64::engine::general_purpose::STANDARD.encode(bits),
                }),
        })
    }
}
