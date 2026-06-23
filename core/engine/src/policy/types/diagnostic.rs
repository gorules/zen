use std::sync::Arc;

use serde::Serialize;

use super::CursorTarget;

pub type Span = (u32, u32);

pub(crate) struct SpanOps;

impl SpanOps {
    pub(crate) fn char_len(s: &str) -> u32 {
        s.chars().count() as u32
    }

    pub(crate) fn replace_at_char_spans(source: &str, spans: &[Span], new_text: &str) -> String {
        let mut sorted = spans.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        sorted.dedup();
        let mut last_end: Option<u32> = None;
        sorted.retain(|&(start, end)| match last_end {
            Some(prev_end) if start < prev_end => false,
            _ => {
                last_end = Some(end);
                true
            }
        });

        let mut out = source.to_string();
        for (char_start, char_end) in sorted.into_iter().rev() {
            let byte_start = out
                .char_indices()
                .nth(char_start as usize)
                .map_or(out.len(), |(b, _)| b);
            let byte_end = out
                .char_indices()
                .nth(char_end as usize)
                .map_or(out.len(), |(b, _)| b);
            out.replace_range(byte_start..byte_end, new_text);
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub message: String,
    pub severity: Severity,
    pub location: DiagnosticLocation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLocation {
    pub policy_path: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_id: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<CursorTarget>,
}

impl DiagnosticLocation {
    pub fn policy(policy_path: Arc<str>) -> Self {
        Self {
            policy_path,
            block_id: None,
            expression_id: None,
            span: None,
            target: None,
        }
    }

    pub fn block(policy_path: Arc<str>, block_id: Arc<str>) -> Self {
        Self {
            policy_path,
            block_id: Some(block_id),
            expression_id: None,
            span: None,
            target: None,
        }
    }

    pub fn expression(
        policy_path: Arc<str>,
        block_id: Arc<str>,
        expression_id: Arc<str>,
        span: Option<Span>,
    ) -> Self {
        Self {
            policy_path,
            block_id: Some(block_id),
            expression_id: Some(expression_id),
            span,
            target: None,
        }
    }

    pub fn with_target(mut self, target: CursorTarget) -> Self {
        self.target = Some(target);
        self
    }

    pub fn maybe_target(self, target: Option<CursorTarget>) -> Self {
        match target {
            Some(t) => self.with_target(t),
            None => self,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiagnosticCode {
    ParseError,

    UndefinedVariable,
    TypeMismatch,
    InvalidExpression,

    EmptyBlock,
    MissingDefaultBranch,
    MixedScope,
    MaxDepthExceeded,

    CyclicDependency,
    DuplicateWriter,
    InvalidWritePath,
    InputOverride,
    SelfReferencingWrite,
    UnreachableEntityRead,
    PartialObjectWrite,
    UnsupportedNestedIteration,

    DataModelCollision,
    UnknownDataModelTarget,
    DuplicateProperty,
    DuplicateEnumValue,
    InvalidName,

    ImportNotFound,
    CircularImport,

    RedundantNullish,
    RepeatedDerivation,
    PreferMatch,
    RedundantTableRow,
    NonDiscriminatingColumn,
    RedundantParentheses,
}

impl DiagnosticCode {
    pub(crate) fn from_expression_diagnostic(
        diag: &zen_expression::intellisense::diagnostic::Diagnostic,
    ) -> DiagnosticCode {
        use zen_expression::intellisense::diagnostic::DiagnosticSource;
        match diag.source {
            DiagnosticSource::Lexer | DiagnosticSource::Parser => DiagnosticCode::ParseError,
            DiagnosticSource::Compiler => DiagnosticCode::InvalidExpression,
            DiagnosticSource::TypeCheck => {
                let msg = diag.message.to_lowercase();
                if msg.contains("left-hand side of `??`") {
                    DiagnosticCode::RedundantNullish
                } else if msg.contains("cannot be applied")
                    || msg.contains("cannot be used to index")
                    || msg.contains("incompatible")
                {
                    DiagnosticCode::TypeMismatch
                } else if msg.contains("not a valid member") {
                    DiagnosticCode::UndefinedVariable
                } else {
                    DiagnosticCode::InvalidExpression
                }
            }
        }
    }
}

impl Diagnostic {
    pub fn error(
        code: DiagnosticCode,
        location: DiagnosticLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Error,
            location,
        }
    }

    pub fn warning(
        code: DiagnosticCode,
        location: DiagnosticLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Warning,
            location,
        }
    }

    pub fn hint(
        code: DiagnosticCode,
        location: DiagnosticLocation,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            severity: Severity::Hint,
            location,
        }
    }

    pub(crate) fn from_expression(
        diag: &zen_expression::intellisense::diagnostic::Diagnostic,
        location: DiagnosticLocation,
    ) -> Self {
        let severity = match diag.severity {
            zen_expression::intellisense::diagnostic::Severity::Error => Severity::Error,
            zen_expression::intellisense::diagnostic::Severity::Warning => Severity::Warning,
            zen_expression::intellisense::diagnostic::Severity::Hint => Severity::Hint,
        };
        Self {
            code: DiagnosticCode::from_expression_diagnostic(diag),
            message: diag.message.clone(),
            severity,
            location: DiagnosticLocation {
                span: location.span.or(Some(diag.span)),
                ..location
            },
        }
    }

    pub fn is_in(&self, policy_path: &Arc<str>) -> bool {
        self.location.policy_path == *policy_path
    }
}
