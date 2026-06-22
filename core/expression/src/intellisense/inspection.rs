use crate::intellisense::IntelliSenseToken;
use crate::variable::VariableType;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionResult {
    pub span: (u32, u32),
    pub kind: VariableType,
    pub label: String,
}

pub(crate) fn inspect_at(
    source: &str,
    pos: u32,
    tokens: &[IntelliSenseToken],
) -> Option<InspectionResult> {
    let token = tokens
        .iter()
        .filter(|t| t.span.0 <= pos && pos <= t.span.1 && t.span.0 < t.span.1)
        .min_by_key(|t| t.span.1 - t.span.0)?;

    let label = source
        .get(token.span.0 as usize..token.span.1 as usize)
        .unwrap_or("")
        .to_string();

    Some(InspectionResult {
        span: token.span,
        kind: token.kind.clone(),
        label,
    })
}
