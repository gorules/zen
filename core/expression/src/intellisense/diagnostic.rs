use crate::compiler::CompilerError;
use crate::intellisense::type_provider::TypesProvider;
use crate::lexer::{LexerError, Operator};
use crate::parser::AstNodeError;
use crate::parser::Node;
use nohash_hasher::BuildNoHashHasher;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use crate::parser::NodeMetadata;

pub type DiagnosticArgs = BTreeMap<&'static str, String>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub span: (u32, u32),
    pub message: String,
    pub severity: Severity,
    pub source: DiagnosticSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub args: DiagnosticArgs,
}

impl Diagnostic {
    pub fn new(
        span: (u32, u32),
        message: String,
        severity: Severity,
        source: DiagnosticSource,
    ) -> Self {
        Self {
            span,
            message,
            severity,
            source,
            code: None,
            args: DiagnosticArgs::new(),
        }
    }

    pub fn incomplete() -> Self {
        let mut diagnostic = Self::new(
            (0, 0),
            "Incomplete expression".to_string(),
            Severity::Error,
            DiagnosticSource::Parser,
        );
        diagnostic.code = Some("expr.two-operands");
        diagnostic
    }

    fn with_code(mut self, code: &'static str, args: &[(&'static str, String)]) -> Self {
        self.code = Some(code);
        self.args = args.iter().cloned().collect();
        self
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

    let (code, args): (&'static str, Vec<(&'static str, String)>) = match err {
        LexerError::UnexpectedEof { .. } => ("expr.unterminated-string", Vec::new()),
        LexerError::UnexpectedSymbol { symbol, .. } => {
            ("expr.unexpected-token", vec![("token", symbol.clone())])
        }
        LexerError::UnmatchedSymbol { symbol, .. } => {
            ("expr.unexpected-token", vec![("token", symbol.to_string())])
        }
    };

    Diagnostic::new(
        span,
        err.to_string(),
        Severity::Error,
        DiagnosticSource::Lexer,
    )
    .with_code(code, &args)
}

fn parser_error_code(
    error: &AstNodeError,
    missing_after: Option<Operator>,
) -> (&'static str, Vec<(&'static str, String)>) {
    if let Some(operator) = missing_after {
        return (
            "expr.missing-value",
            vec![("operator", operator.to_string())],
        );
    }

    match error {
        AstNodeError::UnknownBuiltIn { name, .. } => {
            ("expr.unknown-function", vec![("name", name.to_string())])
        }
        AstNodeError::UnknownMethod { name, .. } => {
            ("expr.unknown-method", vec![("name", name.to_string())])
        }
        AstNodeError::UnexpectedIdentifier { received, .. }
        | AstNodeError::UnexpectedToken { received, .. } => (
            "expr.unexpected-token",
            vec![("token", received.to_string())],
        ),
        AstNodeError::InvalidNumber { number, .. } => {
            ("expr.invalid-number", vec![("number", number.to_string())])
        }
        AstNodeError::InvalidBoolean { boolean, .. } => (
            "expr.unexpected-token",
            vec![("token", boolean.to_string())],
        ),
        AstNodeError::InvalidProperty { .. } | AstNodeError::ExpectedProperty { .. } => {
            ("expr.expected-property", Vec::new())
        }
        AstNodeError::MissingToken { .. }
        | AstNodeError::ExpectedLiteral { .. }
        | AstNodeError::UnexpectedEnd { .. } => ("expr.unexpected-end", Vec::new()),
        AstNodeError::Custom { .. } => ("expr.syntax", Vec::new()),
    }
}

pub(crate) fn collect_parser_diagnostics(ast: &Node, diagnostics: &mut Vec<Diagnostic>) {
    let mut stack: Vec<(&Node, Option<Operator>)> = vec![(ast, None)];
    while let Some((node, missing_after)) = stack.pop() {
        match node {
            Node::Error { error, node: inner } => {
                let (code, args) = parser_error_code(error, missing_after);
                diagnostics.push(
                    Diagnostic::new(
                        node.span().unwrap_or_default(),
                        error.to_string(),
                        Severity::Error,
                        DiagnosticSource::Parser,
                    )
                    .with_code(code, &args),
                );
                if let Some(inner) = inner {
                    stack.push((inner, None));
                }
            }
            Node::Binary {
                left,
                operator,
                right,
            } => {
                let missing = matches!(right, Node::Error { node: None, .. }).then_some(*operator);
                stack.push((right, missing));
                stack.push((left, None));
            }
            Node::Unary { node, .. }
            | Node::Closure { body: node, .. }
            | Node::Parenthesized(node) => stack.push((node, None)),
            Node::Member { node, property } => {
                stack.push((property, None));
                stack.push((node, None));
            }
            Node::Slice { node, from, to } => {
                if let Some(to) = to {
                    stack.push((to, None));
                }
                if let Some(from) = from {
                    stack.push((from, None));
                }
                stack.push((node, None));
            }
            Node::Interval { left, right, .. } => {
                stack.push((right, None));
                stack.push((left, None));
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                stack.push((on_false, None));
                stack.push((on_true, None));
                stack.push((condition, None));
            }
            Node::TemplateString(items) | Node::Array(items) => {
                stack.extend(items.iter().rev().map(|n| (*n, None)));
            }
            Node::Object(pairs) => {
                for (k, v) in pairs.iter().rev() {
                    stack.push((v, None));
                    stack.push((k, None));
                }
            }
            Node::Assignments { list, output } => {
                if let Some(output) = output {
                    stack.push((output, None));
                }
                for (k, v) in list.iter().rev() {
                    stack.push((v, None));
                    stack.push((k, None));
                }
            }
            Node::FunctionCall { arguments, .. } => {
                stack.extend(arguments.iter().rev().map(|n| (*n, None)));
            }
            Node::MethodCall {
                this, arguments, ..
            } => {
                stack.extend(arguments.iter().rev().map(|n| (*n, None)));
                stack.push((this, None));
            }
            Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::Pointer
            | Node::Identifier(_)
            | Node::Root => {}
        }
    }
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

        let mut diagnostic = Diagnostic::new(span, message, severity, DiagnosticSource::TypeCheck);
        if let Some((code, args)) = type_data.code_of(node) {
            diagnostic.code = Some(code);
            diagnostic.args = args.clone();
        }
        collected.borrow_mut().push(diagnostic);
    });
    diagnostics.extend(collected.into_inner());
}

pub(crate) fn compiler_error_to_diagnostic(err: &CompilerError) -> Diagnostic {
    Diagnostic::new(
        (0, 0),
        err.to_string(),
        Severity::Error,
        DiagnosticSource::Compiler,
    )
}
