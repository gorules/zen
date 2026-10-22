use napi_derive::napi;
use serde_json::Value;
use zen_engine::workspace::{self, CursorScope, ExpressionKind};

use crate::expression::json_to_variable_type;
use crate::policy::{
    variable_type_to_json, PolicyCompletion, PolicyDiagnostic, PolicyInspectResult,
};
use crate::slot::{label_resolver, object_mut, parse_role, slot_response_json, to_json, type_json};

#[napi(object)]
pub struct StandaloneExpressionOptions {
    #[napi(ts_type = "PolicyVariableType")]
    pub scope: Value,
    pub unary: Option<bool>,
    #[napi(ts_type = "PolicySlotRole")]
    pub role: Option<String>,
    #[napi(ts_type = "PolicyVariableType | null")]
    pub expected: Option<Value>,
    #[napi(ts_type = "Record<string, Record<string, string>> | null")]
    pub labels: Option<Value>,
    pub strict: Option<bool>,
}

#[napi]
pub struct StandaloneExpression {
    inner: workspace::StandaloneExpression,
}

fn cursor_scope(options: &StandaloneExpressionOptions) -> napi::Result<CursorScope> {
    let unary = options.unary.unwrap_or(false);
    let role = match options.role.as_deref() {
        Some(role) => parse_role(role)?,
        None if unary => parse_role("unary")?,
        None => parse_role("value")?,
    };
    Ok(CursorScope {
        kind: if unary {
            ExpressionKind::Unary
        } else {
            ExpressionKind::Standard
        },
        role,
        scope: json_to_variable_type(&options.scope),
        expected: options.expected.as_ref().map(json_to_variable_type),
        inferred: false,
    })
}

#[napi]
impl StandaloneExpression {
    #[napi(constructor)]
    pub fn new(options: StandaloneExpressionOptions) -> napi::Result<Self> {
        let mut inner = workspace::StandaloneExpression::new(
            cursor_scope(&options)?,
            options.strict.unwrap_or(false),
        );
        inner.set_labels(label_resolver(options.labels.as_ref()));
        Ok(Self { inner })
    }

    #[napi]
    pub fn set_options(&mut self, options: StandaloneExpressionOptions) -> napi::Result<()> {
        self.inner.set_scope(cursor_scope(&options)?);
        self.inner
            .set_labels(label_resolver(options.labels.as_ref()));
        Ok(())
    }

    #[napi]
    pub fn completions(&mut self, text: String, pos: u32) -> Vec<PolicyCompletion> {
        self.inner
            .completions(&text, pos)
            .into_iter()
            .map(|c| PolicyCompletion {
                label: c.label,
                kind: serde_json::to_value(&c.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                detail: c.detail,
                info: c.info,
                follow: c.follow.map(String::from),
            })
            .collect()
    }

    #[napi]
    pub fn inspect(&mut self, text: String, pos: u32) -> Option<PolicyInspectResult> {
        self.inner
            .inspect(&text, pos)
            .map(|result| PolicyInspectResult {
                span: vec![result.span.0, result.span.1],
                kind: variable_type_to_json(&result.kind),
                label: result.label,
                detail: result.detail,
                info: result.info,
            })
    }

    #[napi(ts_return_type = "PolicySlotResponse")]
    pub fn slot(&mut self, text: String, pos: u32) -> napi::Result<Value> {
        slot_response_json(self.inner.slot(&text, pos))
    }

    #[napi]
    pub fn diagnostics(&mut self, text: String) -> Vec<PolicyDiagnostic> {
        self.inner
            .diagnostics(&text)
            .iter()
            .map(PolicyDiagnostic::from)
            .collect()
    }

    #[napi(ts_return_type = "Omit<PolicyExpressionFacts, 'blockId' | 'target'>")]
    pub fn facts(&mut self, text: String) -> napi::Result<Value> {
        let facts = self.inner.facts(&text);
        let mut value = to_json(&facts)?;
        let obj = object_mut(&mut value)?;
        obj.insert("subjectType".into(), type_json(&facts.subject_type));
        obj.insert("expectedType".into(), type_json(&facts.expected_type));
        Ok(value)
    }
}
