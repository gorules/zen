use std::sync::Arc;

use ahash::HashSet;
use serde::{Deserialize, Serialize};
use zen_expression::variable::{Variable, VariableType};

use crate::policy::types::{
    BlockTrace, Cursor, CursorTarget, Diagnostic, DiagnosticCode, ExpressionKind,
};

use crate::policy::ArcStrTrim;

use super::context::{AnalysisContext, ExecutionContext, ExecutionError};
use super::{
    Block, BlockKind, BlockReadPlan, ConditionalReads, ExpressionLocation, ParseContext,
    ReadFlattenFn, WriteSite, WriteTarget,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionDoc {
    #[serde(default)]
    pub key: Arc<str>,
    #[serde(default)]
    pub value: Arc<str>,
}

#[derive(Debug, Clone)]
pub struct ExpressionIr {
    pub id: Arc<str>,
    pub key: Arc<str>,
    pub value: Arc<str>,
}

impl ExpressionIr {
    pub(crate) fn parse(
        id: &Arc<str>,
        doc: &ExpressionDoc,
        policy_path: &Arc<str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Block {
        let mut cx = ParseContext {
            policy_path,
            block_id: id,
            diagnostics,
        };
        let mut key = doc.key.trimmed();
        let value = doc.value.trimmed();

        if key.is_empty() {
            cx.block_warning(
                DiagnosticCode::InvalidWritePath,
                "expression has no target property path",
            );
        } else if let Err(reason) = WriteTarget::validate_path(&key) {
            cx.target_error(
                id,
                CursorTarget::ExpressionKey,
                Some((0, key.chars().count() as u32)),
                DiagnosticCode::InvalidWritePath,
                format!("invalid write path '{key}': {reason}"),
            );
            key = Arc::from("");
        }
        if value.is_empty() {
            cx.block_warning(DiagnosticCode::EmptyBlock, "expression has no value");
        }

        Block {
            id: id.clone(),
            kind: BlockKind::Expression(Arc::new(ExpressionIr {
                id: id.clone(),
                key,
                value,
            })),
        }
    }

    pub(super) fn expressions(&self, block_id: &Arc<str>) -> Vec<ExpressionLocation> {
        ExpressionLocation::try_new(
            block_id.clone(),
            block_id.clone(),
            ExpressionKind::Standard,
            self.value.clone(),
        )
        .into_iter()
        .collect()
    }

    pub(super) fn write_sites(&self) -> Vec<WriteSite> {
        if self.key.is_empty() || self.value.is_empty() {
            return Vec::new();
        }
        let mut contributing = HashSet::default();
        contributing.insert(self.id.clone());
        vec![WriteSite {
            path: self.key.clone(),
            expression_id: Some(self.id.clone()),
            resolved_type: VariableType::Any,
            contributing_expr_ids: contributing,
        }]
    }

    pub(super) fn write_value_expressions(&self, key: &str) -> Vec<Arc<str>> {
        if self.key.as_ref() == key && !self.value.is_empty() {
            vec![self.value.clone()]
        } else {
            Vec::new()
        }
    }

    pub(super) fn read_plan(&self, flatten: &mut ReadFlattenFn) -> BlockReadPlan {
        BlockReadPlan {
            unconditional: BlockReadPlan::dedup(flatten(&self.value, ExpressionKind::Standard)),
            conditional: ConditionalReads::None,
        }
    }

    pub(super) fn write_target(&self, path: &str) -> Option<CursorTarget> {
        (!self.key.is_empty() && self.key.as_ref() == path).then_some(CursorTarget::ExpressionKey)
    }

    pub(super) fn analyze(&self, cx: &mut AnalysisContext) {
        if self.value.is_empty() {
            return;
        }
        let analysis = cx.analyze_standard(&self.value, Some(self.id.clone()));
        if self.key.is_empty() {
            return;
        }
        let instance_source = cx.flow_source(&self.value);
        cx.record_write_sourced(
            self.key.clone(),
            analysis.return_type.clone(),
            None,
            Some(CursorTarget::ExpressionKey),
            instance_source,
        );
    }

    pub(super) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        if self.key.is_empty() || self.value.is_empty() {
            return Ok(BlockTrace::Expression {
                property: self.key.clone(),
                value: Variable::Null,
            });
        }

        let mut isolate = cx.isolate.borrow_mut();
        let result = isolate
            .run_standard(&self.value)
            .map_err(|e| cx.expression_error(&self.value, e))?;
        let traced = cx.trace.then(|| result.deep_clone());
        cx.write(&self.key, result);

        Ok(BlockTrace::Expression {
            property: self.key.clone(),
            value: traced.unwrap_or(Variable::Null),
        })
    }

    pub(super) fn resolve_cursor(
        &self,
        cursor: &Cursor,
        scope: VariableType,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        match &cursor.target {
            CursorTarget::ExpressionKey => {
                (!self.key.is_empty()).then(|| (self.key.clone(), ExpressionKind::Standard, scope))
            }
            CursorTarget::Expression { .. } => (!self.value.is_empty())
                .then(|| (self.value.clone(), ExpressionKind::Standard, scope)),
            _ => None,
        }
    }
}
