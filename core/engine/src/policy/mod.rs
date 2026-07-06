use std::sync::Arc;

pub(crate) const MAX_RECURSION_DEPTH: usize = 32;

pub(crate) mod blocks;
pub(crate) mod db;
pub(crate) mod editor;
pub(crate) mod evaluator;
pub(crate) mod ir;
pub(crate) mod linter;
pub(crate) mod queries;
pub(crate) mod raw;
pub(crate) mod refs;
pub(crate) mod runtime;
mod types;
pub(crate) mod validator;
mod workspace;

pub use raw::{BlockDoc, PolicyDocument};
pub use types::{
    BlockExecution, BlockRef, Completion, ConditionalSchema, Cursor, CursorTarget, DependencyNode,
    Diagnostic, DiagnosticCode, DiagnosticLocation, DiscriminantVariant, DiscriminatedUnion,
    EngineEdit, Entity, EntityField, EvaluateRequest, EvaluationError, EvaluationResult,
    ExpressionKind, FieldOrigin, GuardedProperty, InputProperty, InputValidationError,
    InspectResult, NlExpression, OutputProperty, PrepareRename, PropertyKind, ReferenceKind,
    ReferenceSite, RenameTarget, SchemaFieldKind, SchemaGroup, ScopeRequest, Severity, Span, Trace,
    WriteConflict,
    WriteTrace,
};
pub use workspace::PolicyWorkspace;

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
