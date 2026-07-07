use std::sync::Arc;

use crate::policy::db::Db;
use crate::policy::evaluator::EvalArtifact;
use crate::policy::raw::PolicyDocument;
use zen_expression::nl::NlResult;

use crate::policy::types::{
    Completion, ConditionalSchema, Cursor, DependencyNode, Diagnostic, EngineEdit, Entity,
    EvaluateRequest, EvaluationError, EvaluationResult, Global, InputProperty, InspectResult,
    NlExpression, OutputProperty, PrepareRename, ReferenceSite, RenameTarget, ScopeRequest,
    WriteConflict,
};

pub struct PolicyWorkspace {
    db: Db,
}

impl PolicyWorkspace {
    pub fn new() -> Self {
        Self { db: Db::new() }
    }

    pub fn set_policy(&mut self, path: impl Into<Arc<str>>, document: PolicyDocument) {
        self.db.set_policy(path.into(), Arc::new(document));
    }

    pub fn set_policy_arc(&mut self, path: impl Into<Arc<str>>, document: Arc<PolicyDocument>) {
        self.db.set_policy(path.into(), document);
    }

    pub fn remove_policy(&mut self, path: &str) -> bool {
        self.db.remove_policy(path)
    }

    pub fn policy_paths(&self) -> Vec<Arc<str>> {
        self.db.policy_paths()
    }

    pub fn get_policy(&self, path: &str) -> Option<Arc<PolicyDocument>> {
        self.db.raw_policy(path)
    }

    pub fn evaluate(&self, req: &EvaluateRequest) -> Result<EvaluationResult, EvaluationError> {
        self.db.evaluate(req)
    }

    pub fn enhance_trace(
        &self,
        req: &EvaluateRequest,
    ) -> Result<EvaluationResult, EvaluationError> {
        self.db.enhance_trace(req)
    }

    pub(crate) fn eval_artifact(&self, policy: &str) -> Arc<EvalArtifact> {
        self.db.eval_artifact(policy)
    }

    pub fn entities(&self, req: &ScopeRequest) -> Vec<Entity> {
        self.db.entities(req)
    }

    pub fn globals(&self, req: &ScopeRequest) -> Vec<Global> {
        self.db.globals(req)
    }

    pub fn inputs(&self, req: &ScopeRequest) -> Vec<InputProperty> {
        self.db.inputs(req)
    }

    pub fn outputs(&self, req: &ScopeRequest) -> Vec<OutputProperty> {
        self.db.outputs(req)
    }

    pub fn conditional_schema(&self, req: &ScopeRequest) -> ConditionalSchema {
        self.db.conditional_schema(req)
    }

    pub fn component_members(&self, policy: &str) -> Vec<Arc<str>> {
        self.db.component_members(policy)
    }

    pub fn cross_component_write_conflicts(&self) -> Vec<WriteConflict> {
        self.db.cross_component_write_conflicts()
    }

    pub fn diagnostics(&self, path: &str) -> Vec<Diagnostic> {
        let path_arc: Arc<str> = Arc::from(path);
        (*self.db.policy_diagnostics(&path_arc)).clone()
    }

    pub fn all_diagnostics(&self) -> Vec<Diagnostic> {
        self.db.all_diagnostics()
    }

    pub fn inspect(&self, cursor: &Cursor) -> Option<InspectResult> {
        self.db.inspect(cursor)
    }

    pub fn completions(&self, cursor: &Cursor) -> Vec<Completion> {
        self.db.completions(cursor)
    }

    pub fn nl(&self, policy_path: &str) -> Vec<NlExpression> {
        self.db.nl(policy_path)
    }

    pub fn nl_tokenize(&self, cursor: &Cursor, text: &str) -> Option<NlResult> {
        self.db.nl_tokenize(cursor, text)
    }

    pub fn prepare_rename(&self, cursor: &Cursor) -> Option<PrepareRename> {
        self.db.prepare_rename(cursor)
    }

    pub fn rename(&self, target: &RenameTarget, new_name: &str) -> Vec<EngineEdit> {
        self.db.rename(target, new_name)
    }

    pub fn references(&self, target: &RenameTarget) -> Vec<ReferenceSite> {
        self.db.references(target)
    }

    pub fn input_skeleton(&self, req: &ScopeRequest) -> serde_json::Value {
        self.db.input_skeleton(req)
    }

    pub fn dependencies(&self, target: &str) -> DependencyNode {
        self.db.dependencies(target)
    }
}

impl Default for PolicyWorkspace {
    fn default() -> Self {
        Self::new()
    }
}
