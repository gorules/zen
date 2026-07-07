use std::sync::Arc;

use zen_expression::intellisense::IntelliSense;
use zen_expression::nl::NlResult;
use zen_expression::variable::VariableType;

use crate::policy::types::{CursorTarget, ExpressionKind};

#[derive(Debug, Clone)]
pub struct NlExpression {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub target: CursorTarget,
    pub kind: ExpressionKind,
    pub source: String,
    pub result: NlResult,
}

impl NlExpression {
    pub(crate) fn project(
        is: &mut IntelliSense,
        policy_path: &Arc<str>,
        block_id: &Arc<str>,
        target: CursorTarget,
        kind: ExpressionKind,
        source: &str,
        scope: &VariableType,
    ) -> Self {
        let unary = matches!(kind, ExpressionKind::Unary);
        let mut result = is.nl_tokenize_scoped(block_id, source, unary, scope);
        if unary {
            result.subject_type = Some(scope.get("$"));
        }
        Self {
            policy_path: policy_path.clone(),
            block_id: block_id.clone(),
            target,
            kind,
            source: source.to_string(),
            result,
        }
    }
}
