//! Slot-aware autocomplete: caret classification and literal facts over a partial parse.
use std::rc::Rc;

use bumpalo::Bump;
use serde::Serialize;

use crate::intellisense::scope::IntelliSenseScope;
use crate::intellisense::type_provider::TypesProvider;
use crate::intellisense::{AstMetadata, IntelliSense};
use crate::lexer::{Lexer, QuotationMark, Token};
use crate::parser::{Node, Parser};
use crate::variable::VariableType;

mod classify;
mod literals;
mod operators;
#[cfg(test)]
mod tests;

pub(crate) use literals::NodeTable;

pub type LabelResolver = Rc<dyn Fn(&str, &str) -> Option<String>>;
pub type Span = (u32, u32);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SlotRole {
    Unary,
    Condition,
    Value,
    Path,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValueOption {
    pub value: String,
    pub label: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SlotState {
    Start,
    UnaryStart,
    Value,
    ListElement,
    Range,
    Operator,
    Logical,
    Argument,
    Member,
    Closure,
    InString,
    Path,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Local {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: VariableType,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    pub state: SlotState,
    pub expected: Option<VariableType>,
    pub operand: Option<VariableType>,
    pub options: Vec<ValueOption>,
    pub operators: Vec<&'static str>,
    pub function: Option<String>,
    pub argument: Option<u32>,
    pub replace_span: Span,
    pub in_string: Option<char>,
    pub listed: Vec<String>,
    pub auto_open: bool,
    pub locals: Vec<Local>,
    #[serde(skip)]
    pub can_chain: bool,
    #[serde(skip)]
    pub suppress_completions: bool,
}

impl Slot {
    /// The scalar a field written here must produce: set when the slot asks for a value of a
    /// known scalar type, so a field list can drop the fields that could never fit.
    pub fn wanted_scalar(&self) -> Option<&VariableType> {
        if !matches!(
            self.state,
            SlotState::Value
                | SlotState::UnaryStart
                | SlotState::ListElement
                | SlotState::Range
                | SlotState::Argument
                | SlotState::Member
        ) {
            return None;
        }
        let (t, _) = self.expected.as_ref()?.unwrap_nullable();
        scalar_class(t).map(|_| t)
    }

    pub(crate) fn new(state: SlotState, replace_span: Span) -> Self {
        Self {
            state,
            expected: None,
            operand: None,
            options: Vec::new(),
            operators: Vec::new(),
            function: None,
            argument: None,
            replace_span,
            in_string: None,
            listed: Vec::new(),
            auto_open: false,
            locals: Vec::new(),
            can_chain: false,
            suppress_completions: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LiteralFact {
    Enum {
        span: Span,
        value: String,
        name: Option<String>,
        label: String,
        valid: bool,
        enum_index: u32,
    },
    Date {
        span: Span,
        arg: DateArg,
    },
    Bool {
        span: Span,
        value: bool,
    },
}

impl LiteralFact {
    pub fn span(&self) -> Span {
        match self {
            LiteralFact::Enum { span, .. }
            | LiteralFact::Date { span, .. }
            | LiteralFact::Bool { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DateArg {
    Now,
    Today,
    Literal {
        value: String,
        valid: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        tz: Option<String>,
    },
    Field {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnumTable {
    pub name: Option<String>,
    pub options: Vec<ValueOption>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotResult {
    pub unary: bool,
    pub complete: bool,
    pub slot: Slot,
    pub literals: Vec<LiteralFact>,
    pub enums: Vec<EnumTable>,
}

pub fn encode_string(value: &str) -> Option<String> {
    if !value.contains('"') {
        Some(format!("\"{value}\""))
    } else if !value.contains('\'') {
        Some(format!("'{value}'"))
    } else {
        None
    }
}

/// Whether a hint describes dates, including optional dates and arrays of dates.
pub fn is_date_type(kind: &VariableType) -> bool {
    match kind {
        VariableType::Date => true,
        VariableType::Array(inner) | VariableType::Nullable(inner) => is_date_type(inner),
        _ => false,
    }
}

pub(crate) fn enum_options(
    name: Option<&str>,
    values: &[Rc<str>],
    labels: Option<&LabelResolver>,
) -> Vec<ValueOption> {
    values
        .iter()
        .map(|v| ValueOption {
            value: v.to_string(),
            label: labels
                .zip(name)
                .and_then(|(resolve, n)| resolve(n, v))
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| v.to_string()),
            source: encode_string(v),
        })
        .collect()
}

pub fn subject_enum_options(
    t: &VariableType,
    labels: Option<&LabelResolver>,
) -> Option<Vec<ValueOption>> {
    let (name, values) = enum_values(t)?;
    Some(enum_options(name.as_deref(), &values, labels))
}

pub(crate) type EnumDomain = (Option<Rc<str>>, Vec<Rc<str>>);

pub(crate) const NULL_SOURCE: &str = "null";

pub(crate) fn null_option() -> ValueOption {
    ValueOption {
        value: NULL_SOURCE.to_string(),
        label: NULL_SOURCE.to_string(),
        source: Some(NULL_SOURCE.to_string()),
    }
}

pub(crate) fn is_null_option(option: &ValueOption) -> bool {
    option.value == NULL_SOURCE && option.source.as_deref() == Some(NULL_SOURCE)
}

pub(crate) fn enum_values(t: &VariableType) -> Option<EnumDomain> {
    let mut current = t;
    loop {
        match current {
            VariableType::Enum(name, values) => return Some((name.clone(), values.clone())),
            VariableType::Const(value) => return Some((None, vec![value.clone()])),
            VariableType::Nullable(inner) | VariableType::Array(inner) => current = inner,
            _ => return None,
        }
    }
}

pub(crate) struct Parsed<'a> {
    pub arena: &'a Bump,
    pub source: &'a str,
    pub tokens: &'a [Token<'a>],
    pub open_string: Option<(QuotationMark, u32)>,
    pub ast: &'a Node<'a>,
    pub complete: bool,
    pub metadata: AstMetadata,
    pub types: TypesProvider,
    pub scope: VariableType,
    pub strict: bool,
}

impl<'a> Parsed<'a> {
    pub(crate) fn span_of(&self, node: &Node) -> Span {
        let addr = node as *const Node as usize;
        node.span()
            .or_else(|| self.metadata.get(&addr).map(|m| m.span))
            .unwrap_or_default()
    }

    pub(crate) fn type_of(&self, node: &Node) -> VariableType {
        self.types
            .get_type(node)
            .map(|t| t.kind.clone())
            .unwrap_or(VariableType::Any)
    }

    /// A run re-parsed on its own sees `#` as an identifier, so the pointer type is aliased too.
    pub(crate) fn intellisense_scope(&self, pointer: Option<VariableType>) -> IntelliSenseScope {
        let mut scope = IntelliSenseScope {
            root_data: self.scope.shallow_clone(),
            current_data: self.scope.shallow_clone(),
            ..Default::default()
        };
        match pointer {
            Some(p) => {
                scope.aliases.insert(Rc::from("#"), p.shallow_clone());
                scope.pointer_data = p;
            }
            None => scope.pointer_data = self.scope.shallow_clone(),
        }
        scope
    }
}

fn parse_partial<'a>(
    arena: &'a Bump,
    lexer: &mut Lexer,
    strict: bool,
    source: &'a str,
    unary: bool,
    scope: &VariableType,
) -> Option<Parsed<'a>> {
    let lenient = lexer.tokenize_lenient(arena, source).ok()?;
    let tokens: &'a [Token<'a>] = lenient.tokens.into_bump_slice();
    let parser = Parser::try_new(tokens, arena).ok()?;
    let result = if unary {
        parser.unary().with_metadata().parse()
    } else {
        parser.standard().with_metadata().parse()
    };
    let ast = result.root;
    let complete = result.is_complete;
    let metadata = result.metadata.unwrap_or_default();
    let is_scope = IntelliSenseScope {
        pointer_data: scope.shallow_clone(),
        root_data: scope.shallow_clone(),
        current_data: scope.shallow_clone(),
        ..Default::default()
    };
    let types = TypesProvider::generate(ast, is_scope, strict);

    Some(Parsed {
        arena,
        source,
        tokens,
        open_string: lenient.open_string,
        ast,
        complete,
        metadata,
        types,
        scope: scope.shallow_clone(),
        strict,
    })
}

impl IntelliSense {
    /// Slot at byte `pos` plus literal facts for the whole `source`. `scope` already holds `$` for unary.
    pub fn slot(
        &mut self,
        source: &str,
        pos: u32,
        unary: bool,
        role: SlotRole,
        scope: &VariableType,
        expected: Option<&VariableType>,
    ) -> SlotResult {
        self.arena.reset();
        let pos = clamp_pos(source, pos);
        let Some(parsed) = parse_partial(
            &self.arena,
            &mut self.lexer,
            self.strict,
            source,
            unary,
            scope,
        ) else {
            return SlotResult {
                unary,
                complete: source.trim().is_empty(),
                slot: classify::fallback(
                    source,
                    pos,
                    unary,
                    role,
                    scope,
                    expected,
                    self.labels.as_ref(),
                ),
                literals: Vec::new(),
                enums: Vec::new(),
            };
        };

        let table = NodeTable::build(&parsed, unary, expected);
        let (literals, enums) =
            literals::facts(&parsed, &table, unary, expected, self.labels.as_ref());
        let slot = classify::classify(
            &parsed,
            &table,
            pos,
            unary,
            role,
            expected,
            self.labels.as_ref(),
        );

        SlotResult {
            unary,
            complete: parsed.complete && !parsed.ast.has_error() && parsed.open_string.is_none(),
            slot,
            literals,
            enums,
        }
    }

    /// Closure-bound names visible at byte `pos`, innermost first: `x` for `map(m as x, ...)`,
    /// `#` for an unaliased closure, each with the element type.
    pub fn closure_locals(
        &mut self,
        source: &str,
        pos: u32,
        scope: &VariableType,
    ) -> Vec<(Rc<str>, VariableType)> {
        self.arena.reset();
        let pos = clamp_pos(source, pos);
        let Some(parsed) = parse_partial(
            &self.arena,
            &mut self.lexer,
            self.strict,
            source,
            false,
            scope,
        ) else {
            return Vec::new();
        };
        let table = NodeTable::build(&parsed, false, None);
        classify::closure_locals(&parsed, &table, pos)
    }

    /// Literal facts only (bulk projection, no caret).
    pub fn literals(
        &mut self,
        source: &str,
        unary: bool,
        scope: &VariableType,
        expected: Option<&VariableType>,
    ) -> (Vec<LiteralFact>, Vec<EnumTable>) {
        let (literals, enums, _) = self.literal_analysis(source, unary, scope, expected);
        (literals, enums)
    }

    pub fn literal_analysis(
        &mut self,
        source: &str,
        unary: bool,
        scope: &VariableType,
        expected: Option<&VariableType>,
    ) -> (Vec<LiteralFact>, Vec<EnumTable>, bool) {
        self.arena.reset();
        let Some(parsed) = parse_partial(
            &self.arena,
            &mut self.lexer,
            self.strict,
            source,
            unary,
            scope,
        ) else {
            return (Vec::new(), Vec::new(), source.trim().is_empty());
        };

        let table = NodeTable::build(&parsed, unary, expected);
        let (literals, enums) =
            literals::facts(&parsed, &table, unary, expected, self.labels.as_ref());
        (
            literals,
            enums,
            parsed.complete && !parsed.ast.has_error() && parsed.open_string.is_none(),
        )
    }
}

fn clamp_pos(source: &str, pos: u32) -> u32 {
    let mut pos = (pos as usize).min(source.len());
    while pos > 0 && !source.is_char_boundary(pos) {
        pos -= 1;
    }
    pos as u32
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum ScalarClass {
    Bool,
    Number,
    String,
    Date,
}

fn scalar_class(t: &VariableType) -> Option<ScalarClass> {
    match t {
        VariableType::Bool => Some(ScalarClass::Bool),
        VariableType::Number => Some(ScalarClass::Number),
        VariableType::String | VariableType::Const(_) | VariableType::Enum(..) => {
            Some(ScalarClass::String)
        }
        VariableType::Date => Some(ScalarClass::Date),
        _ => None,
    }
}

/// Whether a field of type `field` can stand where `wanted` is expected: scalars must match in
/// kind, while objects, arrays and untyped values may still lead to a fitting path.
pub fn field_fits(field: &VariableType, wanted: &VariableType) -> bool {
    let (field, _) = field.unwrap_nullable();
    match scalar_class(field) {
        Some(class) => scalar_class(wanted) == Some(class),
        None => !matches!(field, VariableType::Null | VariableType::Interval),
    }
}
