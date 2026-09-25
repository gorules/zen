use std::rc::Rc;
use std::str::FromStr;

use chrono_tz::Tz;

use crate::functions::{ClosureFunction, DateMethod, FunctionKind, InternalFunction, MethodKind};
use crate::lexer::{
    ArithmeticOperator, Bracket, ComparisonOperator, LogicalOperator, Operator, QuotationMark,
    Token, TokenKind,
};
use crate::parser::Node;
use crate::variable::VariableType;
use crate::vm::VmDate;
use crate::Variable;

use super::classify::Classifier;
use super::{
    DateArg, EnumTable, LabelResolver, LiteralFact, Literals, Parsed, SlotRole, Span, ValueOption,
    VariableTypeSlot,
};

struct Entry<'a> {
    node: &'a Node<'a>,
    span: Span,
    expected: Option<VariableType>,
}

pub(crate) struct NodeTable<'a> {
    entries: Vec<Entry<'a>>,
}

impl<'a> NodeTable<'a> {
    pub(crate) fn build(parsed: &Parsed<'a>, unary: bool, expected: Option<&VariableType>) -> Self {
        let root_expected = if unary {
            Some(VariableType::Bool)
        } else {
            expected.map(|e| e.shallow_clone())
        };

        let mut entries = Vec::new();
        let mut stack: Vec<(&'a Node<'a>, Option<VariableType>)> =
            vec![(parsed.ast, root_expected)];

        while let Some((node, expected)) = stack.pop() {
            let mut children: Vec<(&'a Node<'a>, Option<VariableType>)> = Vec::new();

            match node {
                Node::Binary {
                    left,
                    operator,
                    right,
                } => {
                    let (left_expected, right_expected) = match operator {
                        Operator::Comparison(_) => (
                            Self::comparison_operand(parsed, right),
                            Self::comparison_operand(parsed, left),
                        ),
                        Operator::Logical(LogicalOperator::And | LogicalOperator::Or) => {
                            (Some(VariableType::Bool), Some(VariableType::Bool))
                        }
                        Operator::Logical(LogicalOperator::NullishCoalescing) => {
                            let fallback = expected
                                .as_ref()
                                .filter(|e| e.enum_domain().is_some())
                                .cloned()
                                .or_else(|| {
                                    parsed
                                        .type_of(left)
                                        .unwrap_nullable()
                                        .0
                                        .shallow_clone()
                                        .declared()
                                })
                                .or_else(|| expected.clone());
                            (expected.clone(), fallback)
                        }
                        _ => (None, None),
                    };
                    children.push((left, left_expected));
                    children.push((right, right_expected));
                }
                Node::Unary {
                    node: inner,
                    operator,
                } => {
                    let inner_expected = match operator {
                        Operator::Logical(LogicalOperator::Not) => Some(VariableType::Bool),
                        Operator::Arithmetic(
                            ArithmeticOperator::Add | ArithmeticOperator::Subtract,
                        ) => Some(VariableType::Number),
                        _ => None,
                    };
                    children.push((inner, inner_expected));
                }
                Node::Conditional {
                    condition,
                    on_true,
                    on_false,
                } => {
                    children.push((condition, Some(VariableType::Bool)));
                    children.push((on_true, expected.clone()));
                    children.push((on_false, expected.clone()));
                }
                Node::Interval { left, right, .. } => {
                    children.push((left, expected.clone()));
                    children.push((right, expected.clone()));
                }
                Node::Array(items) => {
                    let item_expected = expected.as_ref().map(|t| t.innermost().shallow_clone());
                    for item in items.iter() {
                        children.push((item, item_expected.clone()));
                    }
                }
                Node::Parenthesized(inner) => children.push((inner, expected.clone())),
                Node::Closure { body, .. } => children.push((body, expected.clone())),
                Node::FunctionCall { kind, arguments } => {
                    let expectations =
                        Self::function_expectations(parsed, kind, arguments, expected.as_ref());
                    for (i, arg) in arguments.iter().enumerate() {
                        children.push((arg, expectations.get(i).cloned().flatten()));
                    }
                }
                Node::MethodCall {
                    kind,
                    this,
                    arguments,
                } => {
                    children.push((this, None));
                    let first = Self::method_expects_date(kind).then_some(VariableType::Date);
                    for (i, arg) in arguments.iter().enumerate() {
                        children.push((arg, if i == 0 { first.clone() } else { None }));
                    }
                }
                Node::Member { node, property } => {
                    children.push((node, None));
                    children.push((property, None));
                }
                Node::Slice { node, from, to } => {
                    children.push((node, None));
                    if let Some(from) = from {
                        children.push((from, None));
                    }
                    if let Some(to) = to {
                        children.push((to, None));
                    }
                }
                Node::Object(pairs) => {
                    for (k, v) in pairs.iter() {
                        let field = match (expected.as_ref(), k) {
                            (Some(e), Node::String(key) | Node::Identifier(key)) => {
                                e.get(key).declared()
                            }
                            _ => None,
                        };
                        children.push((k, None));
                        children.push((v, field));
                    }
                }
                Node::Assignments { list, output } => {
                    for (k, v) in list.iter() {
                        children.push((k, None));
                        children.push((v, None));
                    }
                    if let Some(output) = output {
                        children.push((output, expected.clone()));
                    }
                }
                Node::TemplateString(parts) => {
                    for part in parts.iter() {
                        children.push((part, None));
                    }
                }
                Node::Error { node: inner, .. } => {
                    if let Some(inner) = inner {
                        children.push((inner, expected.clone()));
                    }
                }
                Node::Null
                | Node::Bool(_)
                | Node::Number(_)
                | Node::String(_)
                | Node::Pointer
                | Node::Identifier(_)
                | Node::Root => {}
            }

            entries.push(Entry {
                node,
                span: parsed.span_of(node),
                expected,
            });
            stack.extend(children.into_iter().rev());
        }

        Self { entries }
    }
    pub(crate) fn node_at(&self, span: Span) -> Option<&'a Node<'a>> {
        self.entries
            .iter()
            .rfind(|e| e.span == span && !matches!(e.node, Node::Error { .. }))
            .map(|e| e.node)
    }

    fn comparison_operand(parsed: &Parsed, node: &Node) -> Option<VariableType> {
        match (node, parsed.type_of(node)) {
            (Node::String(_), VariableType::Const(_)) => Some(VariableType::String),
            (_, t) => t.declared(),
        }
    }

    fn method_expects_date(kind: &MethodKind) -> bool {
        matches!(
            kind,
            MethodKind::DateMethod(
                DateMethod::IsSame
                    | DateMethod::IsBefore
                    | DateMethod::IsAfter
                    | DateMethod::IsSameOrBefore
                    | DateMethod::IsSameOrAfter
                    | DateMethod::Diff
            )
        )
    }

    fn function_expectations(
        parsed: &Parsed,
        kind: &FunctionKind,
        arguments: &[&Node],
        expected: Option<&VariableType>,
    ) -> Vec<Option<VariableType>> {
        match kind {
            FunctionKind::Closure(cf) => {
                let body_expected = match cf {
                    ClosureFunction::Map | ClosureFunction::FlatMap => None,
                    _ => Some(VariableType::Bool),
                };
                vec![Self::closure_membership(parsed, arguments), body_expected]
            }
            FunctionKind::Internal(InternalFunction::Date) => vec![Some(VariableType::Date)],
            FunctionKind::Internal(InternalFunction::Bool) => {
                vec![expected.map(|e| e.shallow_clone())]
            }
            FunctionKind::Internal(
                InternalFunction::Contains
                | InternalFunction::StartsWith
                | InternalFunction::EndsWith
                | InternalFunction::Matches
                | InternalFunction::FuzzyMatch,
            ) => {
                let needle = match arguments.first().map(|a| parsed.type_of(a)) {
                    Some(haystack @ VariableType::Array(_)) => Some(haystack),
                    _ => None,
                };
                vec![None, needle]
            }
            _ => Vec::new(),
        }
    }

    fn closure_membership(parsed: &Parsed, arguments: &[&Node]) -> Option<VariableType> {
        let Some(Node::Closure { body, alias }) = arguments.get(1) else {
            return None;
        };
        let mut node: &Node = body;
        while let Node::Parenthesized(inner) = node {
            node = inner;
        }
        let Node::Binary {
            left,
            operator: Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn),
            right,
        } = node
        else {
            return None;
        };
        let binds = match left {
            Node::Pointer => alias.is_none(),
            Node::Identifier(name) => *alias == Some(*name),
            _ => false,
        };
        if !binds {
            return None;
        }
        let rhs = parsed.type_of(right);
        rhs.enum_domain()
            .map(|(name, values)| VariableType::Enum(name, values))
    }
}
struct EnumInterner<'l> {
    tables: Vec<EnumTable>,
    labels: Option<&'l LabelResolver>,
}

impl<'l> EnumInterner<'l> {
    fn new(labels: Option<&'l LabelResolver>) -> Self {
        Self {
            tables: Vec::new(),
            labels,
        }
    }

    fn intern(&mut self, name: Option<&Rc<str>>, values: &[Rc<str>]) -> u32 {
        let table = EnumTable {
            name: name.map(|n| n.to_string()),
            options: ValueOption::enum_list(name.map(|n| n.as_ref()), values, self.labels),
        };
        if let Some(index) = self.tables.iter().position(|t| *t == table) {
            return index as u32;
        }
        self.tables.push(table);
        (self.tables.len() - 1) as u32
    }

    fn enum_fact(
        &mut self,
        span: Span,
        value: &str,
        expected: Option<&VariableType>,
    ) -> Option<LiteralFact> {
        let (name, values) = expected?.enum_domain()?;
        Some(LiteralFact::Enum {
            span,
            value: value.to_string(),
            name: name.as_ref().map(|n| n.to_string()),
            label: ValueOption::label(self.labels, name.as_deref(), value),
            valid: values.iter().any(|v| v.as_ref() == value),
            enum_index: self.intern(name.as_ref(), &values),
        })
    }
}

enum TokenLiteral<'a> {
    Str {
        span: Span,
        value: &'a str,
    },
    Bool {
        span: Span,
        value: bool,
    },
    Date {
        span: Span,
        first: usize,
        last: usize,
    },
}

impl<'a> TokenLiteral<'a> {
    fn span(&self) -> Span {
        match self {
            TokenLiteral::Str { span, .. }
            | TokenLiteral::Bool { span, .. }
            | TokenLiteral::Date { span, .. } => *span,
        }
    }

    fn collect(tokens: &[Token<'a>]) -> Vec<Self> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            match t.kind {
                TokenKind::QuotationMark(
                    q @ (QuotationMark::SingleQuote | QuotationMark::DoubleQuote),
                ) => {
                    let closed =
                        tokens
                            .get(i + 1)
                            .zip(tokens.get(i + 2))
                            .filter(|(body, close)| {
                                body.kind == TokenKind::Literal
                                    && close.kind == TokenKind::QuotationMark(q)
                                    && close.span.0 < close.span.1
                            });
                    if let Some((body, close)) = closed {
                        out.push(Self::Str {
                            span: (t.span.0, close.span.1),
                            value: body.value,
                        });
                    }
                    i += 3;
                    continue;
                }
                TokenKind::Boolean(value) => out.push(Self::Bool {
                    span: t.span,
                    value,
                }),
                TokenKind::Literal if t.value == "d" => {
                    let opens = tokens
                        .get(i + 1)
                        .is_some_and(|n| n.kind == TokenKind::Bracket(Bracket::LeftParenthesis));
                    let method = i
                        .checked_sub(1)
                        .is_some_and(|k| tokens[k].kind == TokenKind::Operator(Operator::Dot));
                    if let Some(last) = (opens && !method)
                        .then(|| Self::call_end(tokens, i + 1))
                        .flatten()
                        .map(|last| {
                            let bare = last == i + 2;
                            bare.then(|| Self::today_end(tokens, last))
                                .flatten()
                                .unwrap_or(last)
                        })
                    {
                        out.push(Self::Date {
                            span: (t.span.0, tokens[last].span.1),
                            first: i,
                            last,
                        });
                    }
                }
                _ => {}
            }
            i += 1;
        }
        out
    }

    fn today_end(tokens: &[Token], close: usize) -> Option<usize> {
        let rest = tokens.get(close + 1..close + 8)?;
        let quote = match rest[3].kind {
            TokenKind::QuotationMark(
                q @ (QuotationMark::SingleQuote | QuotationMark::DoubleQuote),
            ) => q,
            _ => return None,
        };
        let shape = rest[0].kind == TokenKind::Operator(Operator::Dot)
            && rest[1].kind == TokenKind::Literal
            && rest[1].value == "startOf"
            && rest[2].kind == TokenKind::Bracket(Bracket::LeftParenthesis)
            && rest[4].kind == TokenKind::Literal
            && rest[4].value == "day"
            && rest[5].kind == TokenKind::QuotationMark(quote)
            && rest[5].span.0 < rest[5].span.1
            && rest[6].kind == TokenKind::Bracket(Bracket::RightParenthesis);
        shape.then_some(close + 7)
    }

    fn call_end(tokens: &[Token], open: usize) -> Option<usize> {
        let mut depth = 0u32;
        for (i, t) in tokens.iter().enumerate().skip(open) {
            match t.kind {
                TokenKind::Bracket(
                    Bracket::LeftParenthesis
                    | Bracket::LeftSquareBracket
                    | Bracket::LeftCurlyBracket,
                ) => depth += 1,
                TokenKind::Bracket(_) => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

impl LiteralFact {
    fn for_bool(span: Span, value: bool, expected: Option<&VariableType>) -> Option<LiteralFact> {
        expected
            .is_some_and(|e| matches!(e.unwrap_nullable().0, VariableType::Bool))
            .then_some(LiteralFact::Bool { span, value })
    }
}

impl DateArg {
    fn is_today(node: &Node) -> bool {
        matches!(
            node,
            Node::MethodCall {
                kind: MethodKind::DateMethod(DateMethod::StartOf),
                this: Node::FunctionCall {
                    kind: FunctionKind::Internal(InternalFunction::Date),
                    arguments: [],
                },
                arguments: [Node::String("day")],
            }
        )
    }

    fn from_arguments(arguments: &[&Node]) -> Option<DateArg> {
        match arguments {
            [] => Some(DateArg::Now),
            [Node::String(value)] => Some(DateArg::Literal {
                value: value.to_string(),
                valid: Self::is_valid(value),
                tz: None,
            }),
            [Node::String(value), Node::String(tz)] => Some(DateArg::Literal {
                value: value.to_string(),
                valid: Tz::from_str(tz).ok().is_some_and(|zone| {
                    VmDate::new(Variable::String((*value).into()), Some(zone)).is_valid()
                }),
                tz: Some(tz.to_string()),
            }),
            [field @ (Node::Identifier(_) | Node::Member { .. })] => Some(DateArg::Field {
                path: Self::field_path(field)?.join("."),
            }),
            _ => None,
        }
    }

    fn from_tokens(parsed: &Parsed, first: usize, last: usize) -> Option<DateArg> {
        match parsed.reparse(first, last)? {
            node if Self::is_today(node) => Some(DateArg::Today),
            Node::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Date),
                arguments,
            } => Self::from_arguments(arguments),
            _ => None,
        }
    }

    fn is_valid(value: &str) -> bool {
        VmDate::new(Variable::String(value.into()), None).is_valid()
    }

    fn field_path(node: &Node) -> Option<Vec<String>> {
        match node {
            Node::Identifier(name) => Some(vec![name.to_string()]),
            Node::Pointer => Some(vec!["#".to_string()]),
            Node::Root => Some(vec!["$root".to_string()]),
            Node::Member { node, property } => {
                let mut base = Self::field_path(node)?;
                match property {
                    Node::String(s) => base.push(s.to_string()),
                    Node::Root => base.push("$root".to_string()),
                    Node::Number(n) if n.is_integer() && !n.is_sign_negative() => {
                        let last = base.pop()?;
                        base.push(format!("{last}[{n}]"));
                    }
                    _ => return None,
                }
                Some(base)
            }
            _ => None,
        }
    }
}

impl Literals {
    pub fn map_spans(mut self, f: impl Fn(Span) -> Span) -> Self {
        self.facts = self
            .facts
            .into_iter()
            .map(|fact| fact.map_span(&f))
            .collect();
        self
    }

    pub(crate) fn empty(source: &str) -> Self {
        Self {
            complete: source.trim().is_empty(),
            facts: Vec::new(),
            enums: Vec::new(),
        }
    }

    pub(crate) fn collect(
        parsed: &Parsed,
        table: &NodeTable,
        unary: bool,
        expected: Option<&VariableType>,
        labels: Option<&LabelResolver>,
    ) -> Self {
        let mut interner = EnumInterner::new(labels);
        let mut facts: Vec<LiteralFact> = Vec::new();
        let open_start = parsed.open_string.map(|(_, start)| start);
        let is_open =
            |span: Span| open_start == Some(span.0) || span.1 > parsed.source.len() as u32;

        for entry in &table.entries {
            let inside_today = facts.iter().any(|f| {
                matches!(f, LiteralFact::Date { arg: DateArg::Today, span }
                    if span.0 <= entry.span.0 && entry.span.1 <= span.1)
            });
            let fact = match entry.node {
                Node::String(value) if !is_open(entry.span) && !inside_today => {
                    interner.enum_fact(entry.span, value, entry.expected.as_ref())
                }
                Node::Bool(value) => {
                    LiteralFact::for_bool(entry.span, *value, entry.expected.as_ref())
                }
                node if DateArg::is_today(node) => Some(LiteralFact::Date {
                    span: entry.span,
                    arg: DateArg::Today,
                }),
                Node::FunctionCall {
                    kind: FunctionKind::Internal(InternalFunction::Date),
                    arguments,
                } if !inside_today => {
                    let open_arg = arguments.first().is_some_and(|a| {
                        matches!(a, Node::String(_)) && is_open(parsed.span_of(a))
                    });
                    if open_arg {
                        None
                    } else {
                        DateArg::from_arguments(arguments).map(|arg| LiteralFact::Date {
                            span: entry.span,
                            arg,
                        })
                    }
                }
                _ => None,
            };
            facts.extend(fact);
        }

        if !parsed.complete || parsed.ast.has_error() {
            let operands =
                Classifier::new(parsed, table, unary, SlotRole::Condition, expected, labels);
            for literal in TokenLiteral::collect(parsed.tokens) {
                let span = literal.span();
                let covered = facts.iter().any(|f| {
                    let s = f.span();
                    s.0 <= span.0 && span.1 <= s.1
                });
                if covered {
                    continue;
                }
                let fact = match literal {
                    TokenLiteral::Str { span, value } => {
                        interner.enum_fact(span, value, operands.expected_at(span.0).as_ref())
                    }
                    TokenLiteral::Bool { span, value } => {
                        LiteralFact::for_bool(span, value, operands.expected_at(span.0).as_ref())
                    }
                    TokenLiteral::Date { span, first, last } => {
                        DateArg::from_tokens(parsed, first, last)
                            .map(|arg| LiteralFact::Date { span, arg })
                    }
                };
                facts.extend(fact);
            }
        }

        facts.sort_by_key(|f| f.span());
        Self {
            complete: parsed.is_complete(),
            facts,
            enums: interner.tables,
        }
    }
}
