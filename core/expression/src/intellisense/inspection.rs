use crate::intellisense::IntelliSenseToken;
use crate::lexer::{Identifier, LenientTokens, QuotationMark, TemplateString, Token, TokenKind};
use crate::variable::VariableType;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionResult {
    pub span: (u32, u32),
    pub kind: VariableType,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
}

impl InspectionResult {
    pub(crate) fn typed(source: &str, span: (u32, u32), kind: VariableType) -> Self {
        Self {
            span,
            kind,
            label: source
                .get(span.0 as usize..span.1 as usize)
                .unwrap_or_default()
                .to_string(),
            detail: None,
            info: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoverWord {
    Name,
    Call { member: bool },
}

pub(crate) struct Hover;

impl Hover {
    pub(crate) fn smallest_token(
        source: &str,
        pos: u32,
        tokens: &[IntelliSenseToken],
    ) -> Option<InspectionResult> {
        let token = tokens
            .iter()
            .filter(|t| t.span.0 <= pos && pos <= t.span.1 && t.span.0 < t.span.1)
            .min_by_key(|t| t.span.1 - t.span.0)?;
        Some(InspectionResult::typed(
            source,
            token.span,
            token.kind.clone(),
        ))
    }

    pub(crate) fn word_at(
        source: &str,
        pos: u32,
        lenient: &LenientTokens,
    ) -> Option<((u32, u32), HoverWord)> {
        if lenient.open_string.is_some_and(|(_, start)| start < pos) {
            return None;
        }
        let tokens = &lenient.tokens;
        let index = tokens
            .iter()
            .position(|t| t.span.0 <= pos && pos <= t.span.1 && Self::is_name(t))?;
        let token = &tokens[index];
        let previous = index.checked_sub(1).and_then(|i| tokens.get(i));
        if token.kind == TokenKind::Literal && previous.is_some_and(Self::opens_string_body) {
            return None;
        }
        let called = source
            .get(token.span.1 as usize..)
            .is_some_and(|rest| rest.trim_start().starts_with('('));
        let member = previous.is_some_and(|t| t.value == ".");
        let word = if called {
            HoverWord::Call { member }
        } else {
            HoverWord::Name
        };
        Some((token.span, word))
    }

    fn is_name(token: &Token) -> bool {
        match token.kind {
            TokenKind::Literal => token
                .value
                .chars()
                .next()
                .is_some_and(|c| !c.is_ascii_digit()),
            TokenKind::Identifier(identifier) => identifier != Identifier::Null,
            _ => false,
        }
    }

    fn opens_string_body(token: &Token) -> bool {
        matches!(
            token.kind,
            TokenKind::QuotationMark(
                QuotationMark::SingleQuote | QuotationMark::DoubleQuote | QuotationMark::Backtick
            ) | TokenKind::TemplateString(TemplateString::ExpressionEnd)
        )
    }
}
