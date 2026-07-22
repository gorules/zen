pub(crate) mod db;
pub(crate) mod editor;
pub(crate) mod graph;
pub(crate) mod search;
pub(crate) mod types;

use std::sync::Arc;

use crate::model::DecisionContent;
use crate::policy::evaluator::EvalArtifact;
use crate::policy::raw::PolicyDocument;
use db::Db;
use zen_expression::nl::NlResult;
use zen_expression::variable::VariableType;

pub use graph::{
    FunctionResolutionRequest, FunctionTypeResolver, GraphAnalysis, GraphNodeAnalysis,
    GraphSignature, GraphTraceMap,
};
pub use types::{
    BlockExecution, BlockRef, BlockTrace, Completion, ConditionTrace, ConditionalSchema, Cursor,
    CursorTarget, DecisionTableExtras, DependencyNode, Diagnostic, DiagnosticCode,
    DiagnosticLocation, Dictionary, DictionaryEntryInfo, DiscriminantVariant, DiscriminatedUnion,
    EngineEdit, Entity, EntityField, EvaluateRequest, EvaluationError, EvaluationResult,
    ExpressionKind, FieldOrigin, GuardedProperty, InputProperty, InputValidationError,
    InspectResult, NlExpression, OutputProperty, PrepareRename, PropertyKind, ReferenceKind,
    ReferenceSite, RenameTarget, SchemaFieldKind, SchemaGroup, ScopeRequest, SearchHit,
    SearchHitKind, Severity, Span, Trace, WriteConflict, WriteTrace,
};

use types::Global;

pub struct Workspace {
    db: Db,
}

impl Workspace {
    pub fn new() -> Self {
        Self { db: Db::new() }
    }

    pub fn set_document(&mut self, path: impl Into<Arc<str>>, document: DecisionContent) {
        self.db.set_document(path.into(), Arc::new(document));
    }

    pub fn set_document_arc(&mut self, path: impl Into<Arc<str>>, document: Arc<DecisionContent>) {
        self.db.set_document(path.into(), document);
    }

    pub fn set_policy(&mut self, path: impl Into<Arc<str>>, document: PolicyDocument) {
        self.db.set_policy(path.into(), Arc::new(document));
    }

    pub fn set_policy_arc(&mut self, path: impl Into<Arc<str>>, document: Arc<PolicyDocument>) {
        self.db.set_policy(path.into(), document);
    }

    pub fn remove_path(&mut self, path: &str) -> bool {
        self.db.remove_document(path)
    }

    pub fn paths(&self) -> Vec<Arc<str>> {
        self.db.document_paths()
    }

    pub fn get_document(&self, path: &str) -> Option<Arc<DecisionContent>> {
        self.db.raw_document(path)
    }

    pub fn get_policy(&self, path: &str) -> Option<Arc<PolicyDocument>> {
        self.db.raw_policy(path)
    }

    pub fn is_graph(&self, path: &str) -> bool {
        self.db.is_graph(path)
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

    pub fn enhance_graph_trace(
        &self,
        document: &Arc<str>,
        trace: &GraphTraceMap,
    ) -> Result<Trace, EvaluationError> {
        self.db.enhance_graph_trace(document, trace)
    }

    pub(crate) fn eval_artifact(&self, policy: &str) -> Arc<EvalArtifact> {
        self.db.eval_artifact(policy)
    }

    pub fn entities(&self, req: &ScopeRequest) -> Vec<Entity> {
        if self.db.is_graph(&req.policy_path) {
            return Vec::new();
        }
        self.db.entities(req)
    }

    pub fn globals(&self, req: &ScopeRequest) -> Vec<Global> {
        if self.db.is_graph(&req.policy_path) {
            return Vec::new();
        }
        self.db.globals(req)
    }

    pub fn dictionaries(&self, req: &ScopeRequest) -> Vec<Dictionary> {
        self.db.dictionaries(req)
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

    pub fn set_function_resolver(
        &mut self,
        resolver: impl Fn(&str, &VariableType) -> Option<String> + 'static,
    ) {
        self.db.set_function_resolver(Some(Box::new(resolver)));
    }

    pub fn function_resolution_requests(&self) -> Vec<FunctionResolutionRequest> {
        self.db.function_resolution_requests()
    }

    pub fn set_function_type(&self, source: &str, input: &VariableType, ts_type: Option<&str>) {
        self.db.set_function_type(source, input, ts_type);
    }

    pub fn graph_analysis(&self, path: &str) -> Option<Arc<GraphAnalysis>> {
        let path_arc: Arc<str> = Arc::from(path);
        self.db.graph_analysis(&path_arc)
    }

    pub fn unchecked_nodes(&self, path: &str) -> Vec<Arc<str>> {
        self.db.graph_unchecked_nodes(path)
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

    pub fn search(&self, query: &str, limit: Option<u32>) -> Vec<SearchHit> {
        self.db.search(query, limit)
    }

    pub fn input_skeleton(&self, req: &ScopeRequest) -> serde_json::Value {
        self.db.input_skeleton(req)
    }

    pub fn dependencies(&self, target: &str) -> DependencyNode {
        self.db.dependencies(target)
    }

    pub fn dependencies_scoped(&self, target: &str, document: Option<&str>) -> DependencyNode {
        if let Some(doc) = document {
            if self.db.is_graph(doc) {
                let doc_arc: Arc<str> = Arc::from(doc);
                return self.db.graph_dependencies(&doc_arc, target);
            }
        }
        self.db.dependencies(target)
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
