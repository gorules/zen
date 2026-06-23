mod assertion;
mod context;
mod decision_table;
mod expression;
mod match_block;
mod property_read;
mod type_check;

use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::VariableType;

use crate::policy::types::{
    BlockTrace, Cursor, CursorTarget, Diagnostic, DiagnosticCode, DiagnosticLocation,
    ExpressionKind, Span,
};

#[derive(Debug, Clone)]
pub struct WriteSite {
    pub path: Arc<str>,
    pub expression_id: Option<Arc<str>>,
    pub resolved_type: VariableType,
    pub contributing_expr_ids: HashSet<Arc<str>>,
}

pub use assertion::{AssertionDoc, AssertionIr};
pub(crate) use context::IntelliSenseSource;
pub use context::{
    AnalysisContext, AnalysisSummary, ExecutionContext, ExecutionError, ExpressionLocation,
    InstanceSource, PropertyRead, SharedIntelliSense, WriteTarget,
};
pub(crate) use decision_table::TableSelection;
pub use decision_table::{DecisionTableDoc, DecisionTableIr};
pub use expression::{ExpressionDoc, ExpressionIr};
pub(crate) use match_block::MatchSelection;
pub use match_block::{MatchDoc, MatchIr};
pub(crate) use property_read::ReadFlattener;

impl ExpressionLocation {
    pub(crate) fn try_new(
        block_id: Arc<str>,
        expression_id: Arc<str>,
        kind: ExpressionKind,
        source: Arc<str>,
    ) -> Option<Self> {
        (!source.is_empty()).then_some(ExpressionLocation {
            block_id,
            expression_id,
            kind,
            source,
        })
    }
}

impl WriteTarget {
    pub(crate) fn validate_path(path: &str) -> Result<(), &'static str> {
        if path.is_empty() {
            return Err("write path is empty");
        }
        for segment in path.split('.') {
            if segment.is_empty() {
                return Err("write path has an empty segment");
            }
            if segment.chars().any(char::is_whitespace) {
                return Err("write path segment contains whitespace");
            }
        }
        Ok(())
    }
}

pub(crate) struct ParseContext<'a> {
    pub(crate) policy_path: &'a Arc<str>,
    pub(crate) block_id: &'a Arc<str>,
    pub(crate) diagnostics: &'a mut Vec<Diagnostic>,
}

impl ParseContext<'_> {
    pub(crate) fn block_error(&mut self, code: DiagnosticCode, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(
            code,
            DiagnosticLocation::block(self.policy_path.clone(), self.block_id.clone()),
            message,
        ));
    }

    pub(crate) fn block_warning(&mut self, code: DiagnosticCode, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::warning(
            code,
            DiagnosticLocation::block(self.policy_path.clone(), self.block_id.clone()),
            message,
        ));
    }

    pub(crate) fn expression_error(
        &mut self,
        expression_id: &Arc<str>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic::error(
            code,
            DiagnosticLocation::expression(
                self.policy_path.clone(),
                self.block_id.clone(),
                expression_id.clone(),
                None,
            ),
            message,
        ));
    }

    pub(crate) fn expression_warning(
        &mut self,
        expression_id: &Arc<str>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic::warning(
            code,
            DiagnosticLocation::expression(
                self.policy_path.clone(),
                self.block_id.clone(),
                expression_id.clone(),
                None,
            ),
            message,
        ));
    }

    pub(crate) fn target_error(
        &mut self,
        expression_id: &Arc<str>,
        target: CursorTarget,
        span: Option<Span>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic::error(
            code,
            DiagnosticLocation::expression(
                self.policy_path.clone(),
                self.block_id.clone(),
                expression_id.clone(),
                span,
            )
            .with_target(target),
            message,
        ));
    }
}

pub(crate) struct BlockReadPlan {
    pub(crate) unconditional: Arc<[Arc<str>]>,
    pub(crate) conditional: ConditionalReads,
}

pub(crate) enum ConditionalReads {
    None,
    Match(Arc<[ArmReads]>),
    DecisionTable(Arc<[CellReads]>),
}

pub(crate) struct ArmReads {
    pub(crate) arm_id: Arc<str>,
    pub(crate) value_reads: Arc<[Arc<str>]>,
}

pub(crate) struct CellReads {
    pub(crate) row_idx: u32,
    pub(crate) col_id: Arc<str>,
    pub(crate) cell_reads: Arc<[Arc<str>]>,
}

pub(crate) type ReadFlattenFn<'a> = dyn FnMut(&Arc<str>, ExpressionKind) -> Vec<Arc<str>> + 'a;

impl BlockReadPlan {
    pub(crate) fn dedup(mut paths: Vec<Arc<str>>) -> Arc<[Arc<str>]> {
        paths.sort();
        paths.dedup();
        Arc::from(paths)
    }

    pub(crate) fn match_arm_reads(&self, arm_id: &str) -> Option<&[Arc<str>]> {
        match &self.conditional {
            ConditionalReads::Match(arms) => arms
                .iter()
                .find(|a| a.arm_id.as_ref() == arm_id)
                .map(|a| a.value_reads.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn cell_reads(&self, row_idx: u32, col_id: &str) -> Vec<Arc<str>> {
        match &self.conditional {
            ConditionalReads::DecisionTable(cells) => cells
                .iter()
                .filter(|c| c.row_idx == row_idx && c.col_id.as_ref() == col_id)
                .flat_map(|c| c.cell_reads.iter().cloned())
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Block {
    pub id: Arc<str>,
    pub kind: BlockKind,
}

#[derive(Debug, Clone)]
pub enum BlockKind {
    Assertion(Arc<AssertionIr>),
    DecisionTable(Arc<DecisionTableIr>),
    Expression(Arc<ExpressionIr>),
    Match(Arc<MatchIr>),
}

impl Block {
    pub fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        self.kind.execute(cx)
    }

    pub fn resolve_cursor(
        &self,
        cursor: &Cursor,
        scope: VariableType,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        self.kind.resolve_cursor(cursor, scope)
    }
}

impl BlockKind {
    pub fn expressions(&self, block_id: &Arc<str>) -> Vec<ExpressionLocation> {
        match self {
            BlockKind::Assertion(a) => a.expressions(block_id),
            BlockKind::DecisionTable(d) => d.expressions(block_id),
            BlockKind::Expression(e) => e.expressions(block_id),
            BlockKind::Match(m) => m.expressions(block_id),
        }
    }

    pub fn write_sites(&self) -> Vec<WriteSite> {
        match self {
            BlockKind::Assertion(a) => a.write_sites(),
            BlockKind::DecisionTable(d) => d.write_sites(),
            BlockKind::Expression(e) => e.write_sites(),
            BlockKind::Match(m) => m.write_sites(),
        }
    }

    pub fn writes(&self) -> Vec<WriteTarget> {
        self.write_sites()
            .into_iter()
            .map(|s| WriteTarget {
                path: s.path,
                resolved_type: s.resolved_type,
                instance_source: None,
            })
            .collect()
    }

    pub fn write_target(&self, path: &str) -> Option<CursorTarget> {
        match self {
            BlockKind::Assertion(a) => a.write_target(path),
            BlockKind::DecisionTable(d) => d.write_target(path),
            BlockKind::Expression(e) => e.write_target(path),
            BlockKind::Match(m) => m.write_target(path),
        }
    }

    pub fn write_value_expressions(&self, key: &str) -> Vec<Arc<str>> {
        match self {
            BlockKind::Expression(e) => e.write_value_expressions(key),
            BlockKind::Match(m) => m.write_value_expressions(key),
            _ => Vec::new(),
        }
    }

    pub fn analyze(&self, cx: &mut AnalysisContext) {
        match self {
            BlockKind::Assertion(a) => a.analyze(cx),
            BlockKind::DecisionTable(d) => d.analyze(cx),
            BlockKind::Expression(e) => e.analyze(cx),
            BlockKind::Match(m) => m.analyze(cx),
        }
    }

    pub fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        match self {
            BlockKind::Assertion(a) => a.execute(cx),
            BlockKind::DecisionTable(d) => d.execute(cx),
            BlockKind::Expression(e) => e.execute(cx),
            BlockKind::Match(m) => m.execute(cx),
        }
    }

    pub(crate) fn read_plan(&self, flatten: &mut ReadFlattenFn) -> BlockReadPlan {
        match self {
            BlockKind::Assertion(a) => a.read_plan(flatten),
            BlockKind::DecisionTable(d) => d.read_plan(flatten),
            BlockKind::Expression(e) => e.read_plan(flatten),
            BlockKind::Match(m) => m.read_plan(flatten),
        }
    }

    pub fn resolve_cursor(
        &self,
        cursor: &Cursor,
        scope: VariableType,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        match self {
            BlockKind::Assertion(a) => a.resolve_cursor(cursor, scope),
            BlockKind::DecisionTable(d) => d.resolve_cursor(cursor, scope),
            BlockKind::Expression(e) => e.resolve_cursor(cursor, scope),
            BlockKind::Match(m) => m.resolve_cursor(cursor, scope),
        }
    }

    pub fn write_keys(&self) -> Vec<(Option<Arc<str>>, Arc<str>)> {
        match self {
            BlockKind::DecisionTable(d) => d.write_keys(),
            _ => self
                .write_sites()
                .into_iter()
                .map(|s| (s.expression_id, s.path))
                .collect(),
        }
    }

    pub fn write_dependency_expr_ids(&self) -> HashMap<Arc<str>, HashSet<Arc<str>>> {
        let mut out: HashMap<Arc<str>, HashSet<Arc<str>>> = HashMap::new();
        for site in self.write_sites() {
            out.entry(site.path)
                .or_default()
                .extend(site.contributing_expr_ids);
        }
        out
    }
}
