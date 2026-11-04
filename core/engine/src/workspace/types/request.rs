use std::sync::Arc;

use zen_expression::variable::Variable;

#[derive(Debug, Clone)]
pub struct EvaluateRequest<T = bool> {
    pub policy_path: Arc<str>,
    pub input: Variable,
    pub goals: Vec<Arc<str>>,
    pub trace: T,
}

#[derive(Debug, Clone)]
pub struct ScopeRequest {
    pub policy_path: Arc<str>,
    pub goals: Vec<Arc<str>>,
}

impl ScopeRequest {
    pub fn for_policy(policy_path: impl Into<Arc<str>>) -> Self {
        Self {
            policy_path: policy_path.into(),
            goals: Vec::new(),
        }
    }
}
