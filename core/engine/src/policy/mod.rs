use std::sync::Arc;

pub(crate) const MAX_RECURSION_DEPTH: usize = 32;

pub(crate) mod blocks;
pub(crate) mod evaluator;
pub(crate) mod ir;
pub(crate) mod linter;
pub(crate) mod queries;
pub(crate) mod raw;
pub(crate) mod refs;
pub(crate) mod runtime;
pub(crate) mod validator;

pub use crate::workspace::{
    BlockExecution, BlockRef, BlockTrace, Completion, ConditionTrace, ConditionalSchema, Cursor,
    CursorTarget, DecisionTableExtras, DependencyNode, Diagnostic, DiagnosticCode,
    DiagnosticLocation, Dictionary, DictionaryEntryInfo, DiscriminantVariant, DiscriminatedUnion,
    EngineEdit, Entity, EntityField, EvaluateRequest, EvaluationError, EvaluationResult,
    ExpressionKind, FieldOrigin, FunctionResolutionRequest, FunctionTypeResolver, GraphAnalysis,
    GraphNodeAnalysis, GraphSignature, GraphTraceMap, GuardedProperty, InputProperty,
    InputValidationError, InspectResult, NlExpression, OutputProperty, PrepareRename, PropertyKind,
    ReferenceKind, ReferenceSite, RenameTarget, SchemaFieldKind, SchemaGroup, ScopeRequest,
    Severity, Span, Trace, Workspace, WriteConflict, WriteTrace,
};
pub use raw::{BlockDoc, PolicyDocument};

pub type PolicyWorkspace = Workspace;

pub(crate) trait ArcStrTrim {
    fn trimmed(&self) -> Self;
}

impl ArcStrTrim for Arc<str> {
    fn trimmed(&self) -> Self {
        let trimmed = self.trim();
        if trimmed.len() == self.len() {
            self.clone()
        } else {
            Arc::from(trimmed)
        }
    }
}
