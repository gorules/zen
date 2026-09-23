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

use classify::Classifier;
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
        ScalarClass::of(t).map(|_| t)
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

    pub fn map_span(mut self, f: impl Fn(Span) -> Span) -> Self {
        match &mut self {
            LiteralFact::Enum { span, .. }
            | LiteralFact::Date { span, .. }
            | LiteralFact::Bool { span, .. } => *span = f(*span),
        }
        self
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
pub struct Literals {
    pub complete: bool,
    #[serde(rename = "literals")]
    pub facts: Vec<LiteralFact>,
    pub enums: Vec<EnumTable>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotResult {
    pub slot: Slot,
    pub literals: Literals,
}

pub(crate) type EnumDomain = (Option<Rc<str>>, Vec<Rc<str>>);

pub trait VariableTypeSlot {
    fn innermost(&self) -> &VariableType;
    fn enum_domain(&self) -> Option<EnumDomain>;
    fn declared(self) -> Option<VariableType>;
}

impl VariableTypeSlot for VariableType {
    fn innermost(&self) -> &VariableType {
        match self {
            VariableType::Array(inner) | VariableType::Nullable(inner) => inner.innermost(),
            other => other,
        }
    }

    fn enum_domain(&self) -> Option<EnumDomain> {
        match self.innermost() {
            VariableType::Enum(name, values) => Some((name.clone(), values.clone())),
            VariableType::Const(value) => Some((None, vec![value.clone()])),
            _ => None,
        }
    }

    fn declared(self) -> Option<VariableType> {
        match self {
            VariableType::Any => None,
            other => Some(other),
        }
    }
}

impl ValueOption {
    pub(crate) const NULL: &'static str = "null";

    pub fn for_type(t: &VariableType, labels: Option<&LabelResolver>) -> Option<Vec<Self>> {
        let (name, values) = t.enum_domain()?;
        Some(Self::enum_list(name.as_deref(), &values, labels))
    }

    pub(crate) fn enum_list(
        name: Option<&str>,
        values: &[Rc<str>],
        labels: Option<&LabelResolver>,
    ) -> Vec<Self> {
        values
            .iter()
            .map(|v| Self {
                value: v.to_string(),
                label: Self::label(labels, name, v),
                source: Self::encode(v),
            })
            .collect()
    }

    pub(crate) fn label(labels: Option<&LabelResolver>, name: Option<&str>, value: &str) -> String {
        labels
            .zip(name)
            .and_then(|(resolve, n)| resolve(n, value))
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| value.to_string())
    }

    pub(crate) fn null() -> Self {
        Self {
            value: Self::NULL.to_string(),
            label: Self::NULL.to_string(),
            source: Some(Self::NULL.to_string()),
        }
    }

    pub(crate) fn is_null(&self) -> bool {
        self.value == Self::NULL && self.source.as_deref() == Some(Self::NULL)
    }

    fn encode(value: &str) -> Option<String> {
        if !value.contains('"') {
            Some(format!("\"{value}\""))
        } else if !value.contains('\'') {
            Some(format!("'{value}'"))
        } else {
            None
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

impl<'a> Parsed<'a> {
    fn new(
        arena: &'a Bump,
        lexer: &mut Lexer,
        strict: bool,
        source: &'a str,
        unary: bool,
        scope: &VariableType,
    ) -> Option<Self> {
        let lenient = lexer.tokenize_lenient(arena, source).ok()?;
        let tokens: &'a [Token<'a>] = lenient.tokens.into_bump_slice();
        let parser = Parser::try_new(tokens, arena).ok()?;
        let result = if unary {
            parser.unary().with_metadata().parse()
        } else {
            parser.standard().with_metadata().parse()
        };
        let is_scope = IntelliSenseScope {
            pointer_data: scope.shallow_clone(),
            root_data: scope.shallow_clone(),
            current_data: scope.shallow_clone(),
            ..Default::default()
        };

        Some(Self {
            arena,
            source,
            tokens,
            open_string: lenient.open_string,
            ast: result.root,
            complete: result.is_complete,
            metadata: result.metadata.unwrap_or_default(),
            types: TypesProvider::generate(result.root, is_scope, strict),
            scope: scope.shallow_clone(),
            strict,
        })
    }

    fn clamp(source: &str, pos: u32) -> u32 {
        let mut pos = (pos as usize).min(source.len());
        while pos > 0 && !source.is_char_boundary(pos) {
            pos -= 1;
        }
        pos as u32
    }

    pub(crate) fn reparse(&self, first: usize, last: usize) -> Option<&'a Node<'a>> {
        let result = Parser::try_new(&self.tokens[first..=last], self.arena)
            .ok()?
            .standard()
            .parse();
        (result.is_complete && !result.root.has_error()).then_some(result.root)
    }

    fn is_complete(&self) -> bool {
        self.complete && !self.ast.has_error() && self.open_string.is_none()
    }
}

impl IntelliSense {
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
        let pos = Parsed::clamp(source, pos);
        let labels = self.labels.as_ref();
        let Some(parsed) = Parsed::new(
            &self.arena,
            &mut self.lexer,
            self.strict,
            source,
            unary,
            scope,
        ) else {
            return SlotResult {
                slot: Classifier::fallback(source, pos, unary, role, scope, expected, labels),
                literals: Literals::empty(source),
            };
        };
        let table = NodeTable::build(&parsed, unary, expected);
        SlotResult {
            slot: Classifier::classify(&parsed, &table, pos, unary, role, expected, labels),
            literals: Literals::collect(&parsed, &table, unary, expected, labels),
        }
    }

    pub fn closure_locals(
        &mut self,
        source: &str,
        pos: u32,
        scope: &VariableType,
    ) -> Vec<(Rc<str>, VariableType)> {
        self.arena.reset();
        let pos = Parsed::clamp(source, pos);
        let Some(parsed) = Parsed::new(
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
        Classifier::closure_locals(&parsed, &table, pos)
    }

    pub fn literals(
        &mut self,
        source: &str,
        unary: bool,
        scope: &VariableType,
        expected: Option<&VariableType>,
    ) -> Literals {
        self.arena.reset();
        let labels = self.labels.as_ref();
        let Some(parsed) = Parsed::new(
            &self.arena,
            &mut self.lexer,
            self.strict,
            source,
            unary,
            scope,
        ) else {
            return Literals::empty(source);
        };
        let table = NodeTable::build(&parsed, unary, expected);
        Literals::collect(&parsed, &table, unary, expected, labels)
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum ScalarClass {
    Bool,
    Number,
    String,
    Date,
}

impl ScalarClass {
    fn of(t: &VariableType) -> Option<Self> {
        match t {
            VariableType::Bool => Some(Self::Bool),
            VariableType::Number => Some(Self::Number),
            VariableType::String | VariableType::Const(_) | VariableType::Enum(..) => {
                Some(Self::String)
            }
            VariableType::Date => Some(Self::Date),
            _ => None,
        }
    }

    pub(crate) fn fits(field: &VariableType, wanted: &VariableType) -> bool {
        let (field, _) = field.unwrap_nullable();
        match Self::of(field) {
            Some(class) => Self::of(wanted) == Some(class),
            None => !matches!(field, VariableType::Null | VariableType::Interval),
        }
    }
}
