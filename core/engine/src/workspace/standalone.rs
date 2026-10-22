use std::sync::Arc;

use serde::Serialize;
use zen_expression::intellisense::completion::Completions;
use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::{LabelResolver, Literals, SlotRole, ValueOption};
use zen_expression::variable::VariableType;

use crate::policy::blocks::IntelliSenseSource;
use crate::workspace::slot::{CursorScope, SlotResponse};
use crate::workspace::types::{
    Completion, Diagnostic, DiagnosticLocation, ExpressionKind, InspectResult, SpanOps,
};

const MAX_PADDING: u32 = 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StandaloneFacts {
    pub source: Arc<str>,
    pub kind: ExpressionKind,
    pub role: SlotRole,
    pub subject_type: Option<VariableType>,
    pub expected_type: Option<VariableType>,
    #[serde(flatten)]
    pub literals: Literals,
    pub subject_options: Vec<ValueOption>,
}

pub struct StandaloneExpression {
    intellisense: IntelliSense,
    scope: CursorScope,
    labels: Option<LabelResolver>,
}

impl StandaloneExpression {
    pub fn new(scope: CursorScope, strict: bool) -> Self {
        Self {
            intellisense: IntelliSense::new().with_strict(strict),
            scope,
            labels: None,
        }
    }

    pub fn set_scope(&mut self, scope: CursorScope) {
        self.scope = scope;
    }

    pub fn set_labels(&mut self, labels: Option<LabelResolver>) {
        self.labels = labels.clone();
        self.intellisense.set_labels(labels);
    }

    pub fn completions(&mut self, text: &str, pos: u32) -> Vec<Completion> {
        let len = SpanOps::char_len(text);
        let cursor = if pos > len.saturating_add(MAX_PADDING) {
            len
        } else {
            pos
        };
        let padded;
        let source: &str = if cursor > len {
            padded = format!("{text}{}", " ".repeat((cursor - len) as usize));
            &padded
        } else {
            text
        };
        let byte = SpanOps::byte_offset(source, cursor) as u32;
        let result = self.intellisense.slot(
            source,
            byte,
            self.scope.is_unary(),
            self.scope.role,
            &self.scope.scope,
            self.scope.expected.as_ref(),
        );
        let mut completions = Completions::from_slot(source, byte, &self.scope.scope, &result.slot);
        completions.retain(|c| c.label != "$root");
        completions
    }

    pub fn inspect(&mut self, text: &str, pos: u32) -> Option<InspectResult> {
        let result = self.intellisense.inspect(
            text,
            SpanOps::byte_offset(text, pos) as u32,
            self.scope.is_unary(),
            self.scope.role,
            &self.scope.scope,
        )?;
        Some(InspectResult {
            span: SpanOps::char_span(text, result.span),
            kind: result.kind,
            label: result.label,
            detail: result.detail,
            info: result.info,
        })
    }

    pub fn slot(&mut self, text: &str, pos: u32) -> SlotResponse {
        SlotResponse::compute(&mut self.intellisense, &self.scope, text, pos)
    }

    pub fn diagnostics(&mut self, text: &str) -> Vec<Diagnostic> {
        if text.trim().is_empty() {
            return Vec::new();
        }
        let source: Arc<str> = Arc::from(text);
        let analysis = IntelliSenseSource::analyze(
            &mut self.intellisense,
            &source,
            self.scope.kind,
            &self.scope.scope,
        );
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| {
                Diagnostic::from_expression(
                    diagnostic,
                    DiagnosticLocation {
                        policy_path: Arc::from(""),
                        block_id: None,
                        expression_id: None,
                        span: Some(SpanOps::char_span(text, diagnostic.span)),
                        target: None,
                    },
                )
            })
            .collect()
    }

    pub fn facts(&mut self, text: &str) -> StandaloneFacts {
        let subject_type = self.scope.subject_type();
        let subject_options = subject_type
            .as_ref()
            .filter(|_| text.trim().is_empty())
            .and_then(|t| ValueOption::for_type(t, self.labels.as_ref()))
            .unwrap_or_default();
        let literals = self
            .intellisense
            .literals(
                text,
                self.scope.is_unary(),
                &self.scope.scope,
                self.scope.literal_expected(),
            )
            .map_spans(|span| SpanOps::char_span(text, span));
        StandaloneFacts {
            source: Arc::from(text),
            kind: self.scope.kind,
            role: self.scope.role,
            subject_type,
            expected_type: self
                .scope
                .expected
                .as_ref()
                .map(VariableType::shallow_clone),
            literals,
            subject_options,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    fn object(fields: &[(&str, VariableType)]) -> VariableType {
        let object = VariableType::empty_object();
        if let VariableType::Object(map) = &object {
            for (key, value) in fields {
                map.borrow_mut().insert(Rc::from(*key), value.clone());
            }
        }
        object
    }

    fn scope() -> CursorScope {
        let transaction = object(&[
            ("amount", VariableType::Number),
            ("country", VariableType::String),
        ]);
        let decision = object(&[
            ("status", VariableType::Number),
            (
                "document",
                VariableType::Enum(None, vec![Rc::from("fraud"), Rc::from("credit")]),
            ),
            ("input", object(&[("transaction", transaction)])),
        ]);
        CursorScope {
            kind: ExpressionKind::Standard,
            role: SlotRole::Condition,
            scope: decision,
            expected: Some(VariableType::Bool),
            inferred: false,
        }
    }

    #[test]
    fn completes_paths_and_functions() {
        let mut e = StandaloneExpression::new(scope(), false);
        let labels: Vec<String> = e
            .completions("input.transaction.", 18)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(labels.contains(&"amount".to_string()), "{labels:?}");
        let top: Vec<String> = e.completions("", 0).into_iter().map(|c| c.label).collect();
        assert!(top.contains(&"status".to_string()), "{top:?}");
        assert!(top.iter().any(|l| l == "d"), "{top:?}");
    }

    #[test]
    fn reports_type_errors_with_char_spans() {
        let mut e = StandaloneExpression::new(scope(), false);
        assert!(e.diagnostics("input.transaction.amount > 10").is_empty());
        let errors = e.diagnostics("input.transaction.amount > 'x'");
        assert!(!errors.is_empty());
        assert!(errors[0].location.span.is_some());
        assert!(!e.diagnostics("status >").is_empty());
    }

    #[test]
    fn inspects_and_slots() {
        let mut e = StandaloneExpression::new(scope(), false);
        let hover = e.inspect("input.transaction.amount > 10", 20).unwrap();
        assert_eq!(hover.kind, VariableType::Number);
        let slot = e.slot("document == ", 12);
        assert_eq!(slot.role, SlotRole::Condition);
        let facts = e.facts("document == 'fraud'");
        assert_eq!(facts.role, SlotRole::Condition);
    }
}
