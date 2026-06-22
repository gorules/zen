use crate::compiler::CompilerError;
use crate::intellisense::type_provider::TypesProvider;
use crate::lexer::LexerError;
use crate::parser::Node;
use nohash_hasher::BuildNoHashHasher;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;

use crate::parser::NodeMetadata;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub span: (u32, u32),
    pub message: String,
    pub severity: Severity,
    pub source: DiagnosticSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSource {
    Lexer,
    Parser,
    TypeCheck,
    Compiler,
}

pub(crate) fn lexer_error_to_diagnostic(err: &LexerError) -> Diagnostic {
    let span = match err {
        LexerError::UnexpectedSymbol { span, .. } => *span,
        LexerError::UnmatchedSymbol { position, .. } => (*position, *position),
        LexerError::UnexpectedEof { position, .. } => (*position, *position),
    };

    Diagnostic {
        span,
        message: err.to_string(),
        severity: Severity::Error,
        source: DiagnosticSource::Lexer,
    }
}

pub(crate) fn collect_parser_diagnostics(ast: &Node, diagnostics: &mut Vec<Diagnostic>) {
    let collected = RefCell::new(Vec::new());
    ast.walk(|node| {
        if let Node::Error { error, .. } = node {
            collected.borrow_mut().push(Diagnostic {
                span: node.span().unwrap_or_default(),
                message: error.to_string(),
                severity: Severity::Error,
                source: DiagnosticSource::Parser,
            });
        }
    });
    diagnostics.extend(collected.into_inner());
}

pub(crate) fn collect_type_diagnostics(
    ast: &Node,
    type_data: &TypesProvider,
    metadata: &HashMap<usize, NodeMetadata, BuildNoHashHasher<usize>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let collected = RefCell::new(Vec::new());
    ast.walk(|node| {
        let Some(type_info) = type_data.get_type(node) else {
            return;
        };

        let Some(error) = &type_info.error else {
            return;
        };

        let addr = node as *const Node as usize;
        let span = node
            .span()
            .or_else(|| metadata.get(&addr).map(|m| m.span))
            .unwrap_or_default();

        let (severity, message) = if let Some(hint) = error.strip_prefix("Hint:") {
            (Severity::Warning, hint.trim().to_string())
        } else if let Some(lint) = error.strip_prefix("Lint:") {
            (Severity::Hint, lint.trim().to_string())
        } else {
            (Severity::Error, error.clone())
        };

        collected.borrow_mut().push(Diagnostic {
            span,
            message,
            severity,
            source: DiagnosticSource::TypeCheck,
        });
    });
    diagnostics.extend(collected.into_inner());
}

pub(crate) fn compiler_error_to_diagnostic(err: &CompilerError) -> Diagnostic {
    Diagnostic {
        span: (0, 0),
        message: err.to_string(),
        severity: Severity::Error,
        source: DiagnosticSource::Compiler,
    }
}
