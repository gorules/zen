use std::sync::Arc;

use zen_expression::lane::{Programs, Session};
use zen_expression::variable::Variable;
use zen_expression::ExpressionKind;

use crate::compiled::exact::policy::Exact;
use crate::policy::blocks::{Block, BlockKind, ExecutionError};
use crate::policy::evaluator::EvalArtifact;
use crate::policy::host::{Evaluate, HostState, Pick, Picked, Site};
use crate::workspace::types::{BlockExecution, BlockRef, BlockTrace, EvaluationError};

pub(crate) struct Host<'a> {
    pub(crate) state: HostState<'a>,
    session: Session,
}

impl<'a> Host<'a> {
    pub(crate) fn new(
        artifact: &'a EvalArtifact,
        store: &'a Variable,
        entry: &'a Arc<str>,
        trace: bool,
        extras: bool,
        programs: &Arc<Programs>,
    ) -> Self {
        Self {
            state: HostState::new(artifact, store, entry, trace, extras),
            session: Session::new().with_programs(Some(programs.clone())),
        }
    }

    pub(crate) fn select(&mut self, owner: &BlockRef, rule: &Block) -> Result<Picked, EvaluationError> {
        self.state.select(owner, rule, &mut self.session)
    }

    pub(crate) fn commit(&mut self, owner: &BlockRef, rule: &Block, picked: Picked) -> Result<(), EvaluationError> {
        self.state.commit(owner, rule, picked, &mut self.session)
    }

    pub(crate) fn executions(self) -> Vec<BlockExecution> {
        self.state.executions()
    }

    pub(crate) fn programs<'r>(rules: impl IntoIterator<Item = &'r Block>) -> Programs {
        let mut sources: Vec<(&'r str, ExpressionKind)> = Vec::new();
        for rule in rules {
            rule.kind.sources(&mut sources);
        }
        Programs::compile(sources)
    }
}

impl BlockKind {
    fn sources<'r>(&'r self, out: &mut Vec<(&'r str, ExpressionKind)>) {
        let standard = |source: &'r Arc<str>| (!source.is_empty()).then(|| (source.as_ref(), ExpressionKind::Standard));
        match self {
            BlockKind::Expression(ir) => out.extend(standard(&ir.value)),
            BlockKind::Assertion(ir) => out.extend(ir.conditions.iter().filter_map(|c| standard(&c.expression))),
            BlockKind::Match(ir) => out.extend(
                ir.arms
                    .iter()
                    .flat_map(|arm| [standard(&arm.condition), standard(&arm.value)])
                    .flatten(),
            ),
            BlockKind::DecisionTable(ir) => {
                for col in &ir.outputs {
                    out.extend(ir.rules.iter().filter_map(|rule| rule.get(&col.id)).filter_map(standard));
                }
                for col in &ir.inputs {
                    let kind = match col.field.as_ref().and_then(standard) {
                        Some(field) => {
                            out.push(field);
                            ExpressionKind::Unary
                        }
                        None => ExpressionKind::Standard,
                    };
                    out.extend(
                        ir.rules
                            .iter()
                            .filter_map(|rule| rule.get(&col.id))
                            .filter(|cell| !cell.is_empty())
                            .map(|cell| (cell.as_ref(), kind.clone())),
                    );
                }
            }
        }
    }
}

impl Evaluate for Session {
    fn environment(&mut self, env: Variable) {
        self.set_environment(env);
    }

    fn select(&mut self, rule: &Block, site: &Site) -> Result<Pick, ExecutionError> {
        let mut exact = Exact { session: self, site };
        match &rule.kind {
            BlockKind::Match(m) => exact.select_match(m).map(Pick::Match),
            BlockKind::DecisionTable(d) => exact.select_table(d).map(Pick::Table),
            BlockKind::Expression(_) | BlockKind::Assertion(_) => Ok(Pick::Unconditional),
        }
    }

    fn commit(&mut self, rule: &Block, site: &Site, pick: &Pick) -> Result<BlockTrace, ExecutionError> {
        let mut exact = Exact { session: self, site };
        match (&rule.kind, pick) {
            (BlockKind::Match(m), Pick::Match(selection)) => exact.commit_match(m, selection),
            (BlockKind::DecisionTable(d), Pick::Table(selection)) => exact.commit_table(d, selection),
            _ => exact.execute(rule),
        }
    }
}
