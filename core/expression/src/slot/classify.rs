use std::rc::Rc;

use crate::functions::{
    FunctionKind, FunctionRegistry, InternalFunction, MethodKind, MethodRegistry,
};
use crate::intellisense::type_provider::TypesProvider;
use crate::lexer::{
    ArithmeticOperator, Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator,
    QuotationMark, TemplateString, Token, TokenKind,
};
use crate::parser::Parser;
use crate::variable::VariableType;

use super::literals::{declared, item_expectation, NodeTable};
use super::operators::{nullable_extras, operators_for, LOGICAL};
use super::{
    enum_values, is_null_option, null_option, subject_enum_options, LabelResolver, Parsed, Slot,
    SlotRole, SlotState, Span,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str { quote: QuotationMark, open: bool },
    TemplateOpen,
    TemplateClose,
    TemplateText,
    ExprStart,
    ExprEnd,
    Ident,
    Ref(Identifier),
    Number,
    Bool,
    Open(Bracket),
    Close(Bracket),
    Op(Operator),
}

#[derive(Debug, Clone, Copy)]
struct Item {
    first: usize,
    last: usize,
    span: Span,
    body: Span,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Call {
        name: usize,
        method: bool,
    },
    Paren,
    List {
        after_in: Option<usize>,
        implicit: bool,
    },
    Index,
    Object,
    Template,
    TemplateExpr,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    open: usize,
    kind: FrameKind,
    commas: u32,
}

struct Ctx<'p, 'a> {
    parsed: Option<&'p Parsed<'a>>,
    table: Option<&'p NodeTable<'a>>,
    source: &'p str,
    items: Vec<Item>,
    unary: bool,
    role: SlotRole,
    scope: &'p VariableType,
    expected: Option<&'p VariableType>,
    labels: Option<&'p LabelResolver>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn classify(
    parsed: &Parsed,
    table: &NodeTable,
    pos: u32,
    unary: bool,
    role: SlotRole,
    expected: Option<&VariableType>,
    labels: Option<&LabelResolver>,
) -> Slot {
    let (role, expected) = effective_role(parsed.source, pos, role, expected);
    let ctx = Ctx::new(parsed, table, unary, role, expected, labels);
    ctx.run(pos)
}

pub(crate) fn fallback(
    source: &str,
    pos: u32,
    unary: bool,
    role: SlotRole,
    scope: &VariableType,
    expected: Option<&VariableType>,
    labels: Option<&LabelResolver>,
) -> Slot {
    let (role, expected) = effective_role(source, pos, role, expected);
    let ctx = Ctx {
        parsed: None,
        table: None,
        source,
        items: Vec::new(),
        unary,
        role,
        scope,
        expected,
        labels,
    };
    ctx.run(pos)
}

/// A path target is only a path while the text before the caret is a bare identifier chain;
/// anything else (a `d(` head, operators, brackets) classifies as a plain value expression.
fn effective_role<'p>(
    source: &str,
    pos: u32,
    role: SlotRole,
    expected: Option<&'p VariableType>,
) -> (SlotRole, Option<&'p VariableType>) {
    if role != SlotRole::Path || is_path_prefix(&source.as_bytes()[..pos as usize]) {
        return (role, expected);
    }
    (SlotRole::Value, None)
}

fn is_path_prefix(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| b.is_ascii_whitespace())
        || bytes.first().is_none_or(|b| is_ident_byte(*b))
            && bytes.iter().all(|b| is_ident_byte(*b) || *b == b'.')
}

/// The classifier's operand state machine, reused for token-level literal facts.
pub(crate) struct Operands<'p, 'a> {
    ctx: Ctx<'p, 'a>,
}

impl<'p, 'a> Operands<'p, 'a> {
    pub(crate) fn new(
        parsed: &'p Parsed<'a>,
        table: &'p NodeTable<'a>,
        unary: bool,
        expected: Option<&'p VariableType>,
        labels: Option<&'p LabelResolver>,
    ) -> Self {
        let ctx = Ctx::new(parsed, table, unary, SlotRole::Condition, expected, labels);
        Self { ctx }
    }

    /// Expectation of the operand starting at byte `start`, following the AST walk's rules:
    /// a comparison operand takes the other side's type and method arguments stay untyped.
    pub(crate) fn expected_at(&self, start: u32) -> Option<VariableType> {
        let ctx = &self.ctx;
        let idx = ctx.items.iter().position(|it| it.span.0 == start)?;
        if let Some(right) = ctx.comparison_after(idx) {
            let end = ctx.run_end(right)?;
            return ctx.operand_type(right, end).and_then(declared);
        }
        let slot = ctx.at(start, idx);
        let method_arg = slot.state == SlotState::Argument
            && ctx
                .frames(idx)
                .last()
                .is_some_and(|f| matches!(f.kind, FrameKind::Call { method: true, .. }));
        if method_arg {
            return None;
        }
        slot.expected
    }
}

fn build_items(tokens: &[Token]) -> Vec<Item> {
    let mut items = Vec::with_capacity(tokens.len());
    let mut templates: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let single = Item {
            first: i,
            last: i,
            span: t.span,
            body: t.span,
            kind: Kind::Ident,
        };
        let kind = match t.kind {
            TokenKind::QuotationMark(
                q @ (QuotationMark::SingleQuote | QuotationMark::DoubleQuote),
            ) => {
                let closed = tokens
                    .get(i + 2)
                    .filter(|c| c.kind == TokenKind::QuotationMark(q));
                match (tokens.get(i + 1), closed) {
                    (Some(body), Some(close)) if body.kind == TokenKind::Literal => {
                        items.push(Item {
                            first: i,
                            last: i + 2,
                            span: (t.span.0, close.span.1),
                            body: body.span,
                            kind: Kind::Str {
                                quote: q,
                                open: close.span.0 == close.span.1,
                            },
                        });
                        i += 3;
                        continue;
                    }
                    _ => Kind::Str {
                        quote: q,
                        open: true,
                    },
                }
            }
            TokenKind::QuotationMark(QuotationMark::Backtick) => {
                let zero_width = t.span.0 == t.span.1;
                match templates.last() {
                    Some(false) => {
                        templates.pop();
                        Kind::TemplateClose
                    }
                    Some(true) if zero_width => {
                        templates.pop();
                        Kind::TemplateClose
                    }
                    _ => {
                        templates.push(false);
                        Kind::TemplateOpen
                    }
                }
            }
            TokenKind::TemplateString(TemplateString::ExpressionStart) => {
                if let Some(top) = templates.last_mut() {
                    *top = true;
                }
                Kind::ExprStart
            }
            TokenKind::TemplateString(TemplateString::ExpressionEnd) => {
                if let Some(top) = templates.last_mut() {
                    *top = false;
                }
                Kind::ExprEnd
            }
            TokenKind::Literal => {
                if templates.last() == Some(&false) {
                    Kind::TemplateText
                } else {
                    Kind::Ident
                }
            }
            TokenKind::Identifier(id) => Kind::Ref(id),
            TokenKind::Boolean(_) => Kind::Bool,
            TokenKind::Number => Kind::Number,
            TokenKind::Bracket(b) => match b {
                Bracket::LeftParenthesis
                | Bracket::LeftSquareBracket
                | Bracket::LeftCurlyBracket => Kind::Open(b),
                _ => Kind::Close(b),
            },
            TokenKind::Operator(op) => Kind::Op(op),
        };
        items.push(Item { kind, ..single });
        i += 1;
    }
    items
}

fn is_operand_end(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Str { .. }
            | Kind::TemplateClose
            | Kind::Ident
            | Kind::Ref(_)
            | Kind::Number
            | Kind::Bool
            | Kind::Close(_)
    )
}

fn is_clause_boundary(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Op(Operator::Logical(_))
            | Kind::Op(Operator::Comma)
            | Kind::Op(Operator::QuestionMark)
            | Kind::Op(Operator::Slice)
            | Kind::Op(Operator::Assign)
            | Kind::Op(Operator::Semi)
            | Kind::Open(Bracket::LeftParenthesis)
            | Kind::Open(Bracket::LeftCurlyBracket)
            | Kind::ExprStart
    )
}

fn is_word(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Ident
            | Kind::Ref(_)
            | Kind::Number
            | Kind::Bool
            | Kind::Op(Operator::Logical(_))
            | Kind::Op(Operator::Comparison(
                ComparisonOperator::In | ComparisonOperator::NotIn
            ))
    )
}

fn is_literal_start(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Str { .. }
            | Kind::TemplateOpen
            | Kind::Number
            | Kind::Bool
            | Kind::Ref(Identifier::Null)
            | Kind::Open(Bracket::LeftSquareBracket)
            | Kind::Op(Operator::Arithmetic(
                ArithmeticOperator::Add | ArithmeticOperator::Subtract
            ))
    )
}

fn is_bool(t: &VariableType) -> bool {
    matches!(t.unwrap_nullable().0, VariableType::Bool)
}

/// A constant type widened to its base type: `"open" == |` expects a string, not `"open"`.
fn widen(t: VariableType) -> VariableType {
    match t {
        VariableType::Const(_) => VariableType::String,
        other => other,
    }
}

fn is_ordered(t: &VariableType) -> bool {
    matches!(
        t.unwrap_nullable().0,
        VariableType::Number | VariableType::Date
    )
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'#'
}

impl<'p, 'a> Ctx<'p, 'a> {
    fn new(
        parsed: &'p Parsed<'a>,
        table: &'p NodeTable<'a>,
        unary: bool,
        role: SlotRole,
        expected: Option<&'p VariableType>,
        labels: Option<&'p LabelResolver>,
    ) -> Self {
        Self {
            parsed: Some(parsed),
            table: Some(table),
            source: parsed.source,
            items: build_items(parsed.tokens),
            unary,
            role,
            scope: &parsed.scope,
            expected,
            labels,
        }
    }

    fn run(&self, pos: u32) -> Slot {
        if self.role == SlotRole::Path {
            return self.path_slot(pos);
        }

        if let Some((idx, quote, replace)) = self.string_at(pos) {
            let item = &self.items[idx];
            let body = if quote == '`' {
                self.text((item.span.1, pos)).to_lowercase()
            } else {
                self.text(item.body).to_lowercase()
            };
            let mut slot = self.at(item.span.0, idx);
            if matches!(slot.state, SlotState::UnaryStart | SlotState::ListElement) {
                self.unlist(&mut slot, self.text(item.body));
            }
            slot.options.retain(|o| !is_null_option(o));
            if !body.is_empty() {
                slot.options.retain(|o| {
                    o.value.to_lowercase().starts_with(&body)
                        || o.label.to_lowercase().starts_with(&body)
                });
            }
            if slot.state == SlotState::Operator {
                slot.auto_open = false;
            }
            slot.state = SlotState::InString;
            slot.in_string = Some(quote);
            slot.replace_span = replace;
            return slot;
        }

        let word = self.word_at(pos);
        let (eff, limit, replace) = match word {
            Some(w) => {
                let first = self.not_in_head(w);
                (
                    self.items[first].span.0,
                    first,
                    (self.items[first].span.0, self.items[w].span.1),
                )
            }
            None => (
                pos,
                self.items
                    .iter()
                    .position(|it| it.span.1 > pos || (it.span.0 == it.span.1 && it.span.0 >= pos))
                    .unwrap_or(self.items.len()),
                (pos, pos),
            ),
        };
        let inside = word.is_none() && self.items.get(limit).is_some_and(|it| it.span.0 < pos);
        let mut slot = self.at(eff, limit);
        slot.replace_span = replace;
        if inside {
            slot.auto_open = false;
        }
        // An operand is due (empty editor or after something): offer fields and functions without a keystroke.
        let operand_due = matches!(
            slot.state,
            SlotState::Start | SlotState::Argument | SlotState::Closure
        );
        if operand_due && word.is_none() && !inside {
            slot.auto_open = true;
        }
        slot
    }

    /// Index of the operand run following a comparison (`==`, `in`, `not in`) after item `idx`.
    fn comparison_after(&self, idx: usize) -> Option<usize> {
        let mut next = idx + 1;
        if self.items.get(next)?.kind == Kind::Op(Operator::Logical(LogicalOperator::Not)) {
            next += 1;
        }
        match self.items.get(next)?.kind {
            Kind::Op(Operator::Comparison(_)) => Some(next + 1),
            _ => None,
        }
    }

    fn text(&self, span: Span) -> &str {
        self.source
            .get(span.0 as usize..span.1 as usize)
            .unwrap_or_default()
    }

    fn path_slot(&self, pos: u32) -> Slot {
        let bytes = self.source.as_bytes();
        let mut start = pos as usize;
        while start > 0 && is_ident_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = pos as usize;
        while end < bytes.len() && is_ident_byte(bytes[end]) {
            end += 1;
        }
        let mut slot = Slot::new(SlotState::Path, (start as u32, end as u32));
        slot.auto_open = self.source.trim().is_empty();
        slot
    }

    /// `not in` lexes as two words until the trailing space arrives; treat them as one.
    fn not_in_head(&self, w: usize) -> usize {
        let is_in = self.items[w].kind == Kind::Op(Operator::Comparison(ComparisonOperator::In));
        match w.checked_sub(1) {
            Some(k)
                if is_in
                    && self.items[k].kind == Kind::Op(Operator::Logical(LogicalOperator::Not)) =>
            {
                k
            }
            _ => w,
        }
    }

    fn unlist(&self, slot: &mut Slot, value: &str) {
        let Some(i) = slot.listed.iter().position(|v| v == value) else {
            return;
        };
        slot.listed.remove(i);
        slot.options = slot
            .operand
            .as_ref()
            .and_then(|s| subject_enum_options(s, self.labels))
            .unwrap_or_default();
        let listed = std::mem::take(&mut slot.listed);
        slot.options.retain(|o| !listed.contains(&o.value));
        slot.listed = listed;
    }

    fn word_at(&self, pos: u32) -> Option<usize> {
        self.items
            .iter()
            .position(|it| it.span.0 < pos && pos <= it.span.1 && is_word(it.kind))
    }

    fn string_at(&self, pos: u32) -> Option<(usize, char, Span)> {
        for (i, it) in self.items.iter().enumerate() {
            if let Kind::Str { quote, .. } = it.kind {
                if it.body.0 <= pos && pos <= it.body.1 {
                    let quote = match quote {
                        QuotationMark::SingleQuote => '\'',
                        QuotationMark::DoubleQuote => '"',
                        QuotationMark::Backtick => '`',
                    };
                    return Some((i, quote, it.span));
                }
            }
        }

        let mut stack: Vec<(usize, bool)> = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if it.span.0 > pos {
                break;
            }
            match it.kind {
                Kind::TemplateOpen if it.span.1 <= pos => stack.push((i, false)),
                Kind::TemplateClose if it.span.1 <= pos && it.span.0 < it.span.1 => {
                    stack.pop();
                }
                Kind::ExprStart if it.span.1 <= pos => {
                    if let Some(top) = stack.last_mut() {
                        top.1 = true;
                    }
                }
                Kind::ExprEnd if it.span.1 <= pos => {
                    if let Some(top) = stack.last_mut() {
                        top.1 = false;
                    }
                }
                _ => {}
            }
        }
        let (open, in_expr) = *stack.last()?;
        if in_expr {
            return None;
        }
        let close = self.template_close(open);
        let end = close
            .map(|c| self.items[c].span.1)
            .unwrap_or(self.source.len() as u32);
        Some((open, '`', (self.items[open].span.0, end)))
    }

    fn template_close(&self, open: usize) -> Option<usize> {
        let mut depth = 0;
        for (i, it) in self.items.iter().enumerate().skip(open) {
            match it.kind {
                Kind::TemplateOpen => depth += 1,
                Kind::TemplateClose => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn frames(&self, limit: usize) -> Vec<Frame> {
        let mut frames: Vec<Frame> = Vec::new();
        for j in 0..limit {
            let before = j.checked_sub(1).map(|k| self.items[k].kind);
            match self.items[j].kind {
                Kind::Open(Bracket::LeftParenthesis) => {
                    let kind = match before {
                        Some(Kind::Ident) => FrameKind::Call {
                            name: j - 1,
                            method: j >= 2 && self.items[j - 2].kind == Kind::Op(Operator::Dot),
                        },
                        _ => FrameKind::Paren,
                    };
                    frames.push(Frame {
                        open: j,
                        kind,
                        commas: 0,
                    });
                }
                Kind::Close(Bracket::RightParenthesis) => {
                    if frames.last().is_some_and(|f| {
                        matches!(f.kind, FrameKind::Call { .. } | FrameKind::Paren)
                    }) {
                        frames.pop();
                    }
                }
                Kind::Open(Bracket::LeftSquareBracket) => {
                    let kind = match before {
                        Some(k) if is_operand_end(k) => FrameKind::Index,
                        Some(Kind::Op(Operator::Comparison(
                            ComparisonOperator::In | ComparisonOperator::NotIn,
                        ))) => FrameKind::List {
                            after_in: Some(j - 1),
                            implicit: false,
                        },
                        other => FrameKind::List {
                            after_in: None,
                            implicit: self.unary
                                && frames.is_empty()
                                && other.is_none_or(is_clause_boundary),
                        },
                    };
                    frames.push(Frame {
                        open: j,
                        kind,
                        commas: 0,
                    });
                }
                Kind::Close(Bracket::RightSquareBracket) => {
                    if frames.last().is_some_and(|f| {
                        matches!(f.kind, FrameKind::List { .. } | FrameKind::Index)
                    }) {
                        frames.pop();
                    }
                }
                Kind::Open(Bracket::LeftCurlyBracket) => frames.push(Frame {
                    open: j,
                    kind: FrameKind::Object,
                    commas: 0,
                }),
                Kind::Close(Bracket::RightCurlyBracket) => {
                    if frames.last().is_some_and(|f| f.kind == FrameKind::Object) {
                        frames.pop();
                    }
                }
                Kind::TemplateOpen => frames.push(Frame {
                    open: j,
                    kind: FrameKind::Template,
                    commas: 0,
                }),
                Kind::TemplateClose => {
                    if frames.last().is_some_and(|f| f.kind == FrameKind::Template) {
                        frames.pop();
                    }
                }
                Kind::ExprStart => frames.push(Frame {
                    open: j,
                    kind: FrameKind::TemplateExpr,
                    commas: 0,
                }),
                Kind::ExprEnd => {
                    if frames
                        .last()
                        .is_some_and(|f| f.kind == FrameKind::TemplateExpr)
                    {
                        frames.pop();
                    }
                }
                Kind::Op(Operator::Comma) => {
                    if let Some(top) = frames.last_mut() {
                        top.commas += 1;
                    }
                }
                _ => {}
            }
        }
        frames
    }

    fn at(&self, eff: u32, limit: usize) -> Slot {
        let frames = self.frames(limit);
        let mut prev = limit.checked_sub(1);

        while let Some(p) = prev {
            let unmatched =
                matches!(self.items[p].kind, Kind::Close(_)) && self.matched_open(p).is_none();
            if !unmatched {
                break;
            }
            prev = p.checked_sub(1);
        }

        let mut signed = false;
        while let Some(p) = prev {
            let sign = matches!(
                self.items[p].kind,
                Kind::Op(Operator::Arithmetic(
                    ArithmeticOperator::Add | ArithmeticOperator::Subtract
                ))
            );
            let unary_sign = sign
                && !p
                    .checked_sub(1)
                    .is_some_and(|k| is_operand_end(self.items[k].kind));
            if !unary_sign {
                break;
            }
            signed = true;
            prev = p.checked_sub(1);
        }

        let gap = prev.is_some_and(|p| self.items[p].span.1 < eff);
        let mut slot = self.after(prev, gap, limit, &frames);
        if signed {
            match slot.state {
                SlotState::UnaryStart => {
                    let subject = self.subject();
                    slot = self.value_slot(Some(subject.shallow_clone()), Some(subject));
                }
                SlotState::Start => slot.expected = Some(VariableType::Number),
                _ => {}
            }
        }
        slot
    }

    fn after(&self, prev: Option<usize>, gap: bool, limit: usize, frames: &[Frame]) -> Slot {
        let Some(p) = prev else {
            return self.head();
        };

        match self.items[p].kind {
            Kind::Op(Operator::Dot) => self.member(p),
            Kind::Op(Operator::Comparison(op)) => self.value_after(p, op),
            Kind::Op(Operator::Range) => self.range_after(p, frames),
            Kind::Open(Bracket::LeftSquareBracket) => self.list_head(limit, frames),
            Kind::Op(Operator::Comma) => match frames.last().map(|f| f.kind) {
                Some(FrameKind::List { .. } | FrameKind::Index) => self.list_head(limit, frames),
                Some(FrameKind::Call { .. }) => self.argument(frames),
                Some(_) => self.start(None),
                None if self.unary => self.unary_start(),
                None => self.start(None),
            },
            Kind::Open(Bracket::LeftParenthesis) => match frames.last().map(|f| f.kind) {
                Some(FrameKind::Call { .. }) => self.argument(frames),
                _ => {
                    let before = p.checked_sub(1).map(|k| self.items[k].kind);
                    if let Some(Kind::Op(Operator::Comparison(
                        ComparisonOperator::In | ComparisonOperator::NotIn,
                    ))) = before
                    {
                        let subject = if self.implicit_subject(p - 1) {
                            Some(self.subject())
                        } else {
                            self.left_operand(p - 1)
                        };
                        return self.range_slot(None, subject);
                    }
                    let unary_interval = self.unary
                        && frames.len() == 1
                        && before.is_none_or(is_clause_boundary)
                        && is_ordered(&self.subject());
                    if unary_interval {
                        return self.range_slot(None, Some(self.subject()));
                    }
                    let mut outer = p;
                    while let Some(k) = outer.checked_sub(1) {
                        if self.items[k].kind != Kind::Open(Bracket::LeftParenthesis) {
                            break;
                        }
                        outer = k;
                    }
                    let before = outer.checked_sub(1).map(|k| self.items[k].kind);
                    let inherited = match before {
                        Some(Kind::Op(Operator::Comparison(_))) => self.left_operand(outer - 1),
                        Some(Kind::Op(Operator::Logical(
                            LogicalOperator::And | LogicalOperator::Or | LogicalOperator::Not,
                        ))) => Some(VariableType::Bool),
                        Some(Kind::Op(Operator::Arithmetic(_))) => {
                            self.left_operand(outer - 1).or(Some(VariableType::Number))
                        }
                        _ => self.whole_expected(frames, limit),
                    };
                    if self.closure_frame(frames).is_some() {
                        self.closure_head(frames)
                    } else {
                        self.start(inherited)
                    }
                }
            },
            Kind::Op(Operator::Logical(LogicalOperator::And | LogicalOperator::Or)) => {
                if frames.is_empty() && self.unary {
                    self.unary_start()
                } else if self.closure_frame(frames).is_some() {
                    self.closure_head(frames)
                } else {
                    self.start(Some(VariableType::Bool))
                }
            }
            Kind::Op(Operator::Logical(LogicalOperator::Not)) => {
                if self.closure_frame(frames).is_some() {
                    self.closure_head(frames)
                } else {
                    self.start(Some(VariableType::Bool))
                }
            }
            Kind::Op(Operator::Logical(LogicalOperator::NullishCoalescing)) => {
                let left = self
                    .left_operand(p)
                    .map(|t| t.unwrap_nullable().0.shallow_clone());
                self.branch_slot(left.or_else(|| self.whole_expected(frames, limit)))
            }
            Kind::Op(Operator::QuestionMark) => {
                self.branch_slot(self.whole_expected(frames, limit))
            }
            Kind::Op(Operator::Slice) => match frames.last().map(|f| f.kind) {
                Some(FrameKind::Index) => self.start(Some(VariableType::Number)),
                _ => self.branch_slot(self.whole_expected(frames, limit)),
            },
            Kind::Op(Operator::Semi) => self.start(self.whole_expected(frames, limit)),
            Kind::Op(Operator::Arithmetic(_)) => {
                let left = self.left_operand(p).map(widen);
                self.start(left)
            }
            Kind::Op(Operator::Assign)
            | Kind::Open(Bracket::LeftCurlyBracket)
            | Kind::ExprStart
            | Kind::TemplateOpen
            | Kind::TemplateText
            | Kind::ExprEnd => self.start(None),
            kind if is_operand_end(kind) => self.after_operand(p, gap, frames),
            _ => self.start(None),
        }
    }

    /// Role expectation seen through branch parens (`? (`, `: (`, `?? (`, leading `(`) and
    /// object literal fields, narrowed by the key of the pair the caret sits in.
    fn whole_expected(&self, frames: &[Frame], limit: usize) -> Option<VariableType> {
        let mut current = self.expected?.shallow_clone();
        for (i, frame) in frames.iter().enumerate() {
            let end = frames.get(i + 1).map(|f| f.open).unwrap_or(limit);
            match frame.kind {
                FrameKind::Paren if self.branch_paren(frame.open) => {}
                FrameKind::List { after_in: None, .. } => {
                    current = item_expectation(Some(&current))?;
                }
                FrameKind::Object => {
                    let key = self.object_key(frame.open, end)?;
                    current = match current.get(key) {
                        VariableType::Any | VariableType::Null => return None,
                        t => t,
                    };
                }
                _ => return None,
            }
        }
        Some(current)
    }

    fn branch_paren(&self, open: usize) -> bool {
        open.checked_sub(1).is_none_or(|k| {
            matches!(
                self.items[k].kind,
                Kind::Op(Operator::QuestionMark)
                    | Kind::Op(Operator::Slice)
                    | Kind::Op(Operator::Logical(LogicalOperator::NullishCoalescing))
                    | Kind::Open(Bracket::LeftParenthesis)
            )
        })
    }

    /// Key of the last `key:` pair opened at depth 0 between `open` and `end`.
    fn object_key(&self, open: usize, end: usize) -> Option<&str> {
        let mut depth = 0u32;
        let mut key = None;
        for j in open + 1..end {
            match self.items[j].kind {
                Kind::Open(_) | Kind::TemplateOpen => depth += 1,
                Kind::Close(_) | Kind::TemplateClose => depth = depth.saturating_sub(1),
                Kind::Op(Operator::Slice) if depth == 0 => {
                    let k = self.items[j - 1];
                    key = match k.kind {
                        Kind::Ident | Kind::Str { open: false, .. } => Some(self.text(k.body)),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        key
    }

    fn matched_open(&self, close: usize) -> Option<usize> {
        let mut depth = 0u32;
        for i in (0..=close).rev() {
            match self.items[i].kind {
                Kind::Close(_) | Kind::TemplateClose => depth += 1,
                Kind::Open(_) | Kind::TemplateOpen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// A branch value (`? x`, `: x`, `?? x`, object field): a value slot when the type has members
    /// or the role is Value, otherwise a plain start with the expectation attached.
    fn branch_slot(&self, expected: Option<VariableType>) -> Slot {
        let enumerable = expected
            .as_ref()
            .is_some_and(|e| subject_enum_options(e, self.labels).is_some());
        if expected.is_some() && (self.role == SlotRole::Value || enumerable) {
            self.value_slot(expected, None)
        } else {
            self.start(expected)
        }
    }

    fn subject(&self) -> VariableType {
        self.scope.get("$")
    }

    fn head(&self) -> Slot {
        if self.unary {
            return self.unary_start();
        }
        match (self.role, self.expected) {
            (SlotRole::Value, Some(expected)) => {
                self.value_slot(Some(expected.shallow_clone()), None)
            }
            _ => self.start(self.expected.map(|e| e.shallow_clone())),
        }
    }

    fn start(&self, expected: Option<VariableType>) -> Slot {
        let mut slot = Slot::new(SlotState::Start, (0, 0));
        slot.expected = expected;
        slot
    }

    fn value_slot(&self, expected: Option<VariableType>, operand: Option<VariableType>) -> Slot {
        let mut slot = Slot::new(SlotState::Value, (0, 0));
        slot.options = expected
            .as_ref()
            .and_then(|e| subject_enum_options(e, self.labels))
            .unwrap_or_default();
        if matches!(expected, Some(VariableType::Nullable(_))) {
            slot.options.push(null_option());
        }
        slot.expected = expected;
        slot.operand = operand;
        slot.auto_open = true;
        slot
    }

    fn unary_start(&self) -> Slot {
        let subject = self.subject();
        let mut slot = Slot::new(SlotState::UnaryStart, (0, 0));
        slot.listed = self.unary_listed();
        slot.options = subject_enum_options(&subject, self.labels).unwrap_or_default();
        slot.options.retain(|o| !slot.listed.contains(&o.value));
        slot.operators = operators_for(&subject, true);
        slot.auto_open = !slot.options.is_empty() || !slot.operators.is_empty();
        slot.expected = Some(subject.shallow_clone());
        slot.operand = Some(subject);
        slot
    }

    /// Type of the operand run ending at item `end`, or the implicit `$` in unary.
    fn left_operand(&self, op: usize) -> Option<VariableType> {
        let end = op.checked_sub(1)?;
        if !is_operand_end(self.items[end].kind) {
            return None;
        }
        let start = self.run_start(end, true);
        self.operand_type(start, end)
    }

    /// Type of a comparison operand run; a bare string literal widens so `"open" == |` expects
    /// a string, while a field or variable typed `"hello"` keeps its literal.
    fn operand_type(&self, start: usize, end: usize) -> Option<VariableType> {
        let t = self.type_of_run(start, end, true)?;
        let literal = start == end && matches!(self.items[start].kind, Kind::Str { .. });
        Some(if literal { widen(t) } else { t })
    }

    fn implicit_subject(&self, op: usize) -> bool {
        self.unary
            && op
                .checked_sub(1)
                .is_none_or(|k| !is_operand_end(self.items[k].kind))
    }

    fn value_after(&self, p: usize, op: ComparisonOperator) -> Slot {
        let left = if self.implicit_subject(p) {
            Some(self.subject())
        } else {
            self.left_operand(p)
        };
        let expected = match (op, &left) {
            (ComparisonOperator::In | ComparisonOperator::NotIn, Some(t)) => {
                Some(VariableType::Array(Rc::new(t.shallow_clone())))
            }
            (_, l) => l.clone(),
        };
        let mut slot = self.value_slot(expected, left);
        if !matches!(op, ComparisonOperator::Equal | ComparisonOperator::NotEqual) {
            slot.options.retain(|o| !is_null_option(o));
        }
        slot
    }

    fn member(&self, p: usize) -> Slot {
        let mut slot = Slot::new(SlotState::Member, (0, 0));
        if let Some(end) = p
            .checked_sub(1)
            .filter(|&k| is_operand_end(self.items[k].kind))
        {
            let start = self.run_start(end, false);
            slot.operand = self.type_of_run(start, end, true);
        }
        slot
    }

    fn list_subject(&self, frame: &Frame) -> Option<VariableType> {
        match frame.kind {
            FrameKind::List {
                after_in: Some(op), ..
            } => {
                if self.implicit_subject(op) {
                    Some(self.subject())
                } else {
                    self.left_operand(op)
                }
            }
            FrameKind::List { implicit: true, .. } => Some(self.subject()),
            FrameKind::List { .. } => {
                let before = frame.open.checked_sub(1).map(|k| self.items[k].kind);
                match before {
                    None => item_expectation(self.expected),
                    Some(Kind::Op(Operator::Comparison(_))) => self.left_operand(frame.open - 1),
                    Some(Kind::Open(Bracket::LeftParenthesis)) => {
                        self.closure_membership(frame.open)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `some([...] as x, x in enumField)`: the collection literal expects that field's enum.
    fn closure_membership(&self, open: usize) -> Option<VariableType> {
        let paren = open.checked_sub(1)?;
        let name = paren.checked_sub(1)?;
        let closure = self.items[paren].kind == Kind::Open(Bracket::LeftParenthesis)
            && self.items[name].kind == Kind::Ident
            && matches!(
                FunctionKind::try_from(self.text(self.items[name].span)),
                Ok(FunctionKind::Closure(_))
            );
        if !closure {
            return None;
        }
        let mut i = self.matched_close(open)? + 1;
        let mut alias = None;
        let as_word = self
            .items
            .get(i)
            .is_some_and(|it| it.kind == Kind::Ident && self.text(it.span) == "as");
        if as_word {
            alias = Some(self.text(self.items.get(i + 1)?.span));
            i += 2;
        }
        if self.items.get(i)?.kind != Kind::Op(Operator::Comma) {
            return None;
        }
        i += 1;
        let binds = match (self.items.get(i)?.kind, alias) {
            (Kind::Ref(Identifier::CallbackReference), None) => true,
            (Kind::Ident, Some(a)) => self.text(self.items[i].span) == a,
            _ => false,
        };
        let right = self.comparison_after(i).filter(|_| binds)?;
        let membership = matches!(
            self.items[right - 1].kind,
            Kind::Op(Operator::Comparison(
                ComparisonOperator::In | ComparisonOperator::NotIn
            ))
        );
        if !membership {
            return None;
        }
        let end = self.run_end(right)?;
        let rhs = self.type_of_run(right, end, true)?;
        enum_values(&rhs).map(|(name, values)| VariableType::Enum(name, values))
    }

    fn matched_close(&self, open: usize) -> Option<usize> {
        let mut depth = 0u32;
        for (i, it) in self.items.iter().enumerate().skip(open) {
            match it.kind {
                Kind::Open(_) | Kind::TemplateOpen => depth += 1,
                Kind::Close(_) | Kind::TemplateClose => {
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

    fn list_head(&self, limit: usize, frames: &[Frame]) -> Slot {
        let Some(frame) = frames.last() else {
            return self.start(None);
        };
        if frame.kind == FrameKind::Index {
            return self.start(Some(VariableType::Number));
        }
        let subject = self.list_subject(frame);
        if let Some(range) = self.range_ahead(limit, frame.open) {
            let partner = self
                .run_end(range + 1)
                .and_then(|end| self.type_of_run(range + 1, end, true));
            return self.range_slot(partner, subject);
        }

        let mut slot = Slot::new(SlotState::ListElement, (0, 0));
        slot.listed = self.list_listed(frame.open);
        slot.options = subject
            .as_ref()
            .and_then(|s| subject_enum_options(s, self.labels))
            .unwrap_or_default();
        slot.options.retain(|o| !slot.listed.contains(&o.value));
        slot.expected = subject.as_ref().and_then(|s| item_expectation(Some(s)));
        slot.operand = subject;
        slot.auto_open = true;
        slot
    }

    fn range_after(&self, p: usize, frames: &[Frame]) -> Slot {
        let bound = self.left_operand(p);
        let subject = frames.last().and_then(|f| self.list_subject(f));
        self.range_slot(bound, subject)
    }

    fn range_slot(&self, bound: Option<VariableType>, subject: Option<VariableType>) -> Slot {
        let mut slot = Slot::new(SlotState::Range, (0, 0));
        let pick = |t: Option<VariableType>| {
            t.filter(|t| {
                matches!(
                    t.unwrap_nullable().0,
                    VariableType::Number | VariableType::Date
                )
            })
        };
        slot.expected = pick(bound)
            .or_else(|| pick(subject))
            .or(Some(VariableType::Number));
        slot
    }

    fn range_ahead(&self, limit: usize, open: usize) -> Option<usize> {
        let mut depth = 0;
        for (i, it) in self.items.iter().enumerate().skip(limit) {
            match it.kind {
                Kind::Open(_) | Kind::TemplateOpen => depth += 1,
                Kind::Close(_) | Kind::TemplateClose => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                Kind::Op(Operator::Range) if depth == 0 => return Some(i),
                Kind::Op(Operator::Comma) if depth == 0 => return None,
                _ => {}
            }
        }
        let _ = open;
        None
    }

    fn closure_frame<'f>(&self, frames: &'f [Frame]) -> Option<&'f Frame> {
        for frame in frames.iter().rev() {
            match frame.kind {
                FrameKind::Paren => continue,
                FrameKind::Call {
                    name,
                    method: false,
                } => {
                    let closure = matches!(
                        FunctionKind::try_from(self.text(self.items[name].span)),
                        Ok(FunctionKind::Closure(_))
                    );
                    return (closure && frame.commas >= 1).then_some(frame);
                }
                _ => return None,
            }
        }
        None
    }

    fn haystack_element(&self, frame: &Frame) -> Option<VariableType> {
        let end = self.run_end(frame.open + 1)?;
        match self.type_of_run(frame.open + 1, end, true)? {
            VariableType::Array(inner) => Some(inner.as_ref().shallow_clone()),
            _ => None,
        }
    }

    fn element_type(&self, frame: &Frame) -> Option<VariableType> {
        let end = self.run_end(frame.open + 1)?;
        let collection = self.type_of_run(frame.open + 1, end, false)?;
        collection.iterator().map(|t| t.as_ref().shallow_clone())
    }

    fn closure_head(&self, frames: &[Frame]) -> Slot {
        let Some(frame) = self.closure_frame(frames) else {
            return self.start(Some(VariableType::Bool));
        };
        let FrameKind::Call { name, .. } = frame.kind else {
            return self.start(None);
        };
        let element = self.element_type(frame);
        let mut slot = Slot::new(SlotState::Closure, (0, 0));
        slot.options = element
            .as_ref()
            .and_then(|e| subject_enum_options(e, self.labels))
            .unwrap_or_default();
        slot.expected = element.clone();
        slot.operand = element;
        slot.function = Some(self.text(self.items[name].span).to_string());
        slot.argument = Some(1);
        slot
    }

    fn argument(&self, frames: &[Frame]) -> Slot {
        let Some(frame) = frames.last() else {
            return self.start(None);
        };
        let FrameKind::Call { name, method } = frame.kind else {
            return self.start(None);
        };
        let name_text = self.text(self.items[name].span);
        let index = frame.commas;

        let mut slot = Slot::new(SlotState::Argument, (0, 0));
        slot.function = Some(name_text.to_string());
        slot.argument = Some(index);

        if method {
            let Ok(kind) = MethodKind::try_from(name_text) else {
                return self.start(None);
            };
            slot.expected = MethodRegistry::get_definition(&kind)
                .and_then(|d| d.param_type(index as usize + 1));
            if let Some(end) = name
                .checked_sub(2)
                .filter(|&k| is_operand_end(self.items[k].kind))
            {
                let start = self.run_start(end, false);
                slot.operand = self.type_of_run(start, end, true);
            }
        } else {
            let Ok(kind) = FunctionKind::try_from(name_text) else {
                return self.start(None);
            };
            slot.expected = match &kind {
                FunctionKind::Closure(_) if index >= 1 => return self.closure_head(frames),
                FunctionKind::Closure(_) => Some(VariableType::Array(Rc::new(VariableType::Any))),
                FunctionKind::Internal(InternalFunction::Date) if index == 0 => {
                    Some(VariableType::Date)
                }
                FunctionKind::Internal(InternalFunction::Contains) if index == 1 => {
                    self.haystack_element(frame).or_else(|| {
                        FunctionRegistry::get_definition(&kind).and_then(|d| d.param_type(1))
                    })
                }
                _ => FunctionRegistry::get_definition(&kind)
                    .and_then(|d| d.param_type(index as usize)),
            };
            slot.auto_open = name_text == "d" && index == 0;
        }

        slot.options = slot
            .expected
            .as_ref()
            .and_then(|e| subject_enum_options(e, self.labels))
            .unwrap_or_default();
        slot
    }

    fn after_operand(&self, p: usize, gap: bool, frames: &[Frame]) -> Slot {
        let start = self.run_start(p, true);
        let before = start.checked_sub(1).map(|k| self.items[k].kind);
        let operand = self.type_of_run(start, p, true);
        let at_clause = before.is_none_or(is_clause_boundary);
        let literal_clause = self.unary
            && frames.is_empty()
            && at_clause
            && is_literal_start(self.items[start].kind);
        let bool_clause = at_clause && operand.as_ref().is_some_and(is_bool);
        let date_clause = self.unary
            && frames.is_empty()
            && at_clause
            && matches!(self.subject().unwrap_nullable().0, VariableType::Date)
            && operand
                .as_ref()
                .is_some_and(|t| matches!(t.unwrap_nullable().0, VariableType::Date));
        let after_comparison = matches!(before, Some(Kind::Op(Operator::Comparison(_))));

        if after_comparison || literal_clause || bool_clause || date_clause {
            let mut slot = Slot::new(SlotState::Logical, (0, 0));
            slot.expected = Some(VariableType::Bool);
            slot.operators = LOGICAL.to_vec();
            let nullable = !after_comparison && matches!(operand, Some(VariableType::Nullable(_)));
            if nullable {
                slot.operators.extend(nullable_extras(self.unary));
            }
            slot.auto_open = gap;
            return slot;
        }

        let mut slot = Slot::new(SlotState::Operator, (0, 0));
        slot.operators = operators_for(operand.as_ref().unwrap_or(&VariableType::Any), false);
        slot.operand = operand;
        slot.auto_open = gap && !slot.operators.is_empty();
        slot
    }

    fn primary_start(&self, k: usize) -> usize {
        let mut depth = 0u32;
        let mut i = k;
        let mut start = k;
        loop {
            let kind = self.items[i].kind;
            let include = match kind {
                Kind::Close(_) | Kind::TemplateClose => {
                    depth += 1;
                    true
                }
                Kind::Open(_) | Kind::TemplateOpen => {
                    if depth == 0 {
                        false
                    } else {
                        depth -= 1;
                        true
                    }
                }
                _ if depth > 0 => true,
                Kind::Str { .. }
                | Kind::Ident
                | Kind::Ref(_)
                | Kind::Number
                | Kind::Bool
                | Kind::Op(Operator::Dot) => true,
                _ => false,
            };
            if !include {
                break;
            }
            start = i;
            if i == 0 {
                break;
            }
            i -= 1;
        }
        start
    }

    fn run_start(&self, k: usize, extend: bool) -> usize {
        let mut start = self.primary_start(k);
        if !extend {
            return start;
        }
        while let Some(before) = start.checked_sub(1) {
            let Kind::Op(Operator::Arithmetic(arith)) = self.items[before].kind else {
                break;
            };
            let binary = before
                .checked_sub(1)
                .is_some_and(|k| is_operand_end(self.items[k].kind));
            if binary {
                start = self.primary_start(before - 1);
                continue;
            }
            if matches!(
                arith,
                ArithmeticOperator::Add | ArithmeticOperator::Subtract
            ) {
                start = before;
            }
            break;
        }
        start
    }

    fn run_end(&self, k: usize) -> Option<usize> {
        let mut depth = 0u32;
        let mut end = None;
        for (i, it) in self.items.iter().enumerate().skip(k) {
            let include = match it.kind {
                Kind::Open(_) | Kind::TemplateOpen => {
                    depth += 1;
                    true
                }
                Kind::Close(_) | Kind::TemplateClose => {
                    if depth == 0 {
                        false
                    } else {
                        depth -= 1;
                        true
                    }
                }
                _ if depth > 0 => true,
                Kind::Str { .. }
                | Kind::Ident
                | Kind::Ref(_)
                | Kind::Number
                | Kind::Bool
                | Kind::Op(Operator::Dot) => true,
                Kind::Op(Operator::Arithmetic(
                    ArithmeticOperator::Add | ArithmeticOperator::Subtract,
                )) => end.is_none(),
                _ => false,
            };
            if !include {
                break;
            }
            end = Some(i);
        }
        end
    }

    fn type_of_run(&self, start: usize, end: usize, with_pointer: bool) -> Option<VariableType> {
        let span = (self.items[start].span.0, self.items[end].span.1);
        let parsed = self.parsed?;
        if let Some(node) = self.table.and_then(|t| t.node_at(span)) {
            return Some(parsed.type_of(node));
        }

        let tokens = &parsed.tokens[self.items[start].first..=self.items[end].last];
        let result = Parser::try_new(tokens, parsed.arena)
            .ok()?
            .standard()
            .parse();
        if !result.is_complete || result.root.has_error() {
            return None;
        }
        let pointer = if with_pointer {
            self.pointer_type(start)
        } else {
            None
        };
        let types = TypesProvider::generate(
            result.root,
            parsed.intellisense_scope(pointer),
            parsed.strict,
        );
        types.get_type(result.root).map(|t| t.kind.clone())
    }

    fn pointer_type(&self, at: usize) -> Option<VariableType> {
        let frames = self.frames(at);
        let frame = self.closure_frame(&frames)?;
        self.element_type(frame)
    }

    fn list_listed(&self, open: usize) -> Vec<String> {
        let mut listed = Vec::new();
        let mut depth = 0u32;
        for it in self.items.iter().skip(open + 1) {
            match it.kind {
                Kind::Open(_) | Kind::TemplateOpen => depth += 1,
                Kind::Close(_) | Kind::TemplateClose => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Kind::Str { open: false, .. } if depth == 0 => {
                    listed.push(self.text(it.body).to_string());
                }
                _ => {}
            }
        }
        listed
    }

    fn unary_listed(&self) -> Vec<String> {
        let mut listed = Vec::new();
        let mut depth = 0u32;
        let mut list_open = false;
        let mut before: Option<Kind> = None;
        for it in self.items.iter() {
            match it.kind {
                Kind::Open(b) => {
                    if depth == 0 {
                        list_open = b == Bracket::LeftSquareBracket
                            && before.is_none_or(|k| {
                                is_clause_boundary(k)
                                    || matches!(
                                        k,
                                        Kind::Op(Operator::Comparison(
                                            ComparisonOperator::In | ComparisonOperator::NotIn
                                        ))
                                    )
                            });
                    }
                    depth += 1;
                }
                Kind::TemplateOpen => depth += 1,
                Kind::Close(_) | Kind::TemplateClose => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        list_open = false;
                    }
                }
                Kind::Str { open: false, .. } => {
                    let clause_value = depth == 0
                        && before.is_none_or(|k| {
                            is_clause_boundary(k)
                                || k == Kind::Op(Operator::Comparison(ComparisonOperator::Equal))
                        });
                    if clause_value || (depth == 1 && list_open) {
                        listed.push(self.text(it.body).to_string());
                    }
                }
                _ => {}
            }
            before = Some(it.kind);
        }
        listed
    }
}
