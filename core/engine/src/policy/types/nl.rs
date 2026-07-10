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
        Self::project_expected(is, policy_path, block_id, target, kind, source, scope, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn project_expected(
        is: &mut IntelliSense,
        policy_path: &Arc<str>,
        block_id: &Arc<str>,
        target: CursorTarget,
        kind: ExpressionKind,
        source: &str,
        scope: &VariableType,
        expected: Option<&VariableType>,
    ) -> Self {
        let unary = matches!(kind, ExpressionKind::Unary);
        let mut result = is.nl_tokenize_scoped(block_id, source, unary, scope, expected);
        if unary {
            let subject = scope.get("$");
            result.subject_options = is.nl_subject_options(&subject);
            result.subject_type = Some(subject);
        } else if let Some(expected) = expected {
            result.subject_options = is.nl_subject_options(expected);
            result.subject_type = Some(expected.shallow_clone());
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
