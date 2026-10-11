use crate::compiled::exact::index::Candidates;
use super::{Ctx, Row};
use crate::decision_graph::tracer::{DecisionTableNodeTrace, DecisionTableRowTrace};
use crate::nodes::decision_table::index::TableIndex;
use crate::nodes::{NodeResponse, NodeResult};
use ahash::HashMap;
use fixedbitset::FixedBitSet;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::Session;
use zen_expression::variable::ToVariable;
use zen_types::decision::{DecisionTableContent, DecisionTableHitPolicy, DecisionTableInputField};
use zen_types::variable::Variable;

pub(crate) struct RowTable<'a> {
    ctx: Ctx<'a, DecisionTableNodeTrace>,
    content: &'a DecisionTableContent,
    index: Option<&'a TableIndex>,
}

enum RowResult {
    Output(Variable),
    WithTrace {
        output: Variable,
        reference_map: HashMap<Rc<str>, Variable>,
        rule: HashMap<Rc<str>, Rc<str>>,
    },
}

type Rule = HashMap<Arc<str>, Arc<str>>;
type RowCells = (Option<Variable>, Vec<(usize, Variable)>);
type TraceParts = (HashMap<Rc<str>, Variable>, HashMap<Rc<str>, Rc<str>>);

impl<'a> RowTable<'a> {
    pub(crate) fn new(row: Row<'a>, content: &'a DecisionTableContent, index: Option<&'a TableIndex>) -> Self {
        Self {
            ctx: Ctx::new(row),
            content,
            index,
        }
    }

    pub(crate) fn handle(self) -> NodeResult {
        let has_collect_columns = self.content.outputs.iter().any(|output| output.write_path().1);
        match self.content.hit_policy {
            DecisionTableHitPolicy::First if has_collect_columns => self.handle_first_hit_collect(),
            DecisionTableHitPolicy::First => self.handle_first_hit(),
            DecisionTableHitPolicy::Collect => self.handle_collect(),
        }
    }

    fn handle_first_hit(mut self) -> NodeResult {
        let mut session = self.ctx.row.session();

        if !self.ctx.row.trace {
            let index = self.index;
            let candidates = index.and_then(|ix| Self::candidate_rows(ix, &self.content.inputs, &mut session));
            let pruner = candidates.as_ref().and(index);
            for (row_idx, rule) in self.content.rules.iter().enumerate() {
                if candidates.as_ref().is_some_and(|c| !c.contains(row_idx)) {
                    continue;
                }
                let pruned = pruner.map(|ix| (ix, row_idx));
                if let Some(RowResult::Output(output)) = self.evaluate_row(rule, &mut session, pruned) {
                    return self.ctx.success(output);
                }
            }
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }

        let hit = self.content.rules.iter().enumerate().find_map(|(index, rule)| {
            match self.evaluate_row(rule, &mut session, None)? {
                RowResult::WithTrace {
                    output,
                    reference_map,
                    rule,
                } => Some((index, output, reference_map, rule)),
                RowResult::Output(output) => Some((index, output, Default::default(), Default::default())),
            }
        });

        match hit {
            Some((index, output, reference_map, rule)) => {
                self.ctx.trace(|t| {
                    *t = DecisionTableNodeTrace::FirstHit(DecisionTableRowTrace {
                        reference_map,
                        index,
                        rule,
                    })
                });
                self.ctx.success(output)
            }
            None => Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            }),
        }
    }

    fn handle_collect(mut self) -> NodeResult {
        let mut outputs = Vec::new();
        let mut traces = Vec::new();
        let mut session = self.ctx.row.session();

        let table_index = (!self.ctx.row.trace).then_some(self.index).flatten();
        let candidates = table_index.and_then(|ix| Self::candidate_rows(ix, &self.content.inputs, &mut session));
        let pruner = candidates.as_ref().and(table_index);

        for (index, rule) in self.content.rules.iter().enumerate() {
            if candidates.as_ref().is_some_and(|c| !c.contains(index)) {
                continue;
            }
            let pruned = pruner.map(|ix| (ix, index));
            if let Some(result) = self.evaluate_row(rule, &mut session, pruned) {
                match result {
                    RowResult::Output(output) => {
                        outputs.push(output);
                    }
                    RowResult::WithTrace {
                        output,
                        reference_map,
                        rule,
                    } => {
                        outputs.push(output);
                        traces.push(DecisionTableRowTrace {
                            index,
                            rule,
                            reference_map,
                        });
                    }
                }
            }
        }

        self.ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });

        self.ctx.success(Variable::from_array(outputs))
    }

    pub(crate) fn passes(rule: &Rule, input: &DecisionTableInputField, session: &mut Session) -> bool {
        let Some(rule_value) = rule.get(&input.id) else {
            return true;
        };
        if rule_value.is_empty() {
            return true;
        }
        match &input.field {
            None => session
                .run_standard(rule_value)
                .ok()
                .and_then(|result| result.as_bool())
                .unwrap_or(false),
            Some(field) => {
                if session.set_reference(field).is_err() {
                    return false;
                }
                session.run_unary(rule_value).unwrap_or(false)
            }
        }
    }

    fn candidate_rows(index: &TableIndex, inputs: &[DecisionTableInputField], session: &mut Session) -> Option<FixedBitSet> {
        let narrowed: Result<_, std::convert::Infallible> = Candidates::narrow(index, inputs, |_, field| {
            Ok(session.set_reference(field).ok().and_then(|_| session.get_reference(field)))
        });
        narrowed.ok().flatten()
    }

    fn handle_first_hit_collect(mut self) -> NodeResult {
        let mut session = self.ctx.row.session();

        let table_index = (!self.ctx.row.trace).then_some(self.index).flatten();
        let candidates = table_index.and_then(|ix| Self::candidate_rows(ix, &self.content.inputs, &mut session));
        let pruner = candidates.as_ref().and(table_index);

        let mut scalars: Option<Variable> = None;
        let mut collected: Vec<Vec<Variable>> = vec![Vec::new(); self.content.outputs.len()];
        let mut matched = false;
        let mut traces = Vec::new();

        for (row_idx, rule) in self.content.rules.iter().enumerate() {
            if candidates.as_ref().is_some_and(|c| !c.contains(row_idx)) {
                continue;
            }
            let pruned = pruner.map(|ix| (ix, row_idx));
            if !self.row_matches(rule, &mut session, pruned) {
                continue;
            }

            let Some((row_scalars, row_collects)) = self.evaluate_row_cells(rule, &mut session, scalars.is_none()) else {
                continue;
            };
            matched = true;
            if let Some(row_scalars) = row_scalars {
                scalars = Some(row_scalars);
            }
            for (column_idx, value) in row_collects {
                collected[column_idx].push(value);
            }

            if self.ctx.row.trace {
                let (reference_map, rule_trace) = self.row_trace_parts(rule, &mut session);
                traces.push(DecisionTableRowTrace {
                    index: row_idx,
                    reference_map,
                    rule: rule_trace,
                });
            }
        }

        if !matched {
            return Ok(NodeResponse {
                output: Variable::Null,
                trace_data: None,
            });
        }

        let output = scalars.unwrap_or_else(Variable::empty_object);
        for (column_idx, column) in self.content.outputs.iter().enumerate() {
            let (path, collect) = column.write_path();
            if !collect || path.is_empty() {
                continue;
            }
            let values = std::mem::take(&mut collected[column_idx]);
            output.dot_insert(path, Variable::from_array(values));
        }

        self.ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });
        self.ctx.success(output)
    }

    fn row_matches(&self, rule: &Rule, session: &mut Session, pruned: Option<(&TableIndex, usize)>) -> bool {
        for (col_idx, input) in self.content.inputs.iter().enumerate() {
            let Some(rule_value) = rule.get(&input.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }
            if pruned.is_some_and(|(ix, row_idx)| ix.decides(col_idx, row_idx)) {
                if let Some(field) = &input.field {
                    let _ = session.set_reference(field);
                }
                continue;
            }

            let passed = match &input.field {
                None => session
                    .run_standard(rule_value)
                    .ok()
                    .and_then(|result| result.as_bool())
                    .unwrap_or(false),
                Some(field) => session.set_reference(field).is_ok() && session.run_unary(rule_value).unwrap_or(false),
            };
            if !passed {
                return false;
            }
        }
        true
    }

    fn evaluate_row_cells(
        &self,
        rule: &Rule,
        session: &mut Session,
        include_scalars: bool,
    ) -> Option<RowCells> {
        let scalars = include_scalars.then(Variable::empty_object);
        let mut collects = Vec::new();
        for (column_idx, output) in self.content.outputs.iter().enumerate() {
            let (path, collect) = output.write_path();
            if path.is_empty() || (!collect && !include_scalars) {
                continue;
            }
            let Some(rule_value) = rule.get(&output.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let value = session.run_standard(rule_value).ok()?.deep_clone();
            if collect {
                collects.push((column_idx, value));
            } else if let Some(scalars) = &scalars {
                scalars.dot_insert(path, value);
            }
        }
        Some((scalars, collects))
    }

    fn row_trace_parts(&self, rule: &Rule, session: &mut Session) -> TraceParts {
        let id_str = Rc::<str>::from("_id");
        let description_str = Rc::<str>::from("_description");

        let rule_id = match rule.get(id_str.as_ref()) {
            Some(rid) => Rc::<str>::from(rid.deref()),
            None => Rc::from(""),
        };

        let mut expressions: HashMap<Rc<str>, Rc<str>> = Default::default();
        let mut reference_map: HashMap<Rc<str>, Variable> = Default::default();

        expressions.insert(id_str.clone(), rule_id.clone());
        if let Some(description) = rule.get(description_str.as_ref()) {
            expressions.insert(description_str.clone(), Rc::from(description.deref()));
        }

        for input in self.content.inputs.iter() {
            let Some(rule_value) = rule.get(input.id.deref()) else {
                continue;
            };
            let Some(input_field) = &input.field else {
                continue;
            };

            if let Some(reference) = session.get_reference(input_field.deref()) {
                reference_map.insert(Rc::from(input_field.deref()), reference);
            } else if let Ok(reference) = session.run_standard(input_field.deref()) {
                reference_map.insert(Rc::from(input_field.deref()), reference);
            }

            let input_identifier = format!("{input_field}[{}]", input.id);
            expressions.insert(Rc::from(input_identifier.as_str()), Rc::from(rule_value.deref()));
        }

        (reference_map, expressions)
    }

    fn evaluate_row(&self, rule: &Rule, session: &mut Session, pruned: Option<(&TableIndex, usize)>) -> Option<RowResult> {
        if !self.row_matches(rule, session, pruned) {
            return None;
        }

        let outputs = Variable::empty_object();
        for output in self.content.outputs.iter() {
            let (path, _) = output.write_path();
            if path.is_empty() {
                continue;
            }
            let Some(rule_value) = rule.get(&output.id) else {
                continue;
            };
            if rule_value.is_empty() {
                continue;
            }

            let res = session.run_standard(rule_value).ok()?;
            outputs.dot_insert(path, res.deep_clone());
        }

        if !self.ctx.row.trace {
            return Some(RowResult::Output(outputs));
        }

        let (reference_map, expressions) = self.row_trace_parts(rule, session);
        Some(RowResult::WithTrace {
            output: outputs.to_variable(),
            reference_map,
            rule: expressions,
        })
    }
}
