use std::rc::Rc;

use crate::functions::{
    FunctionKind, FunctionRegistry, InternalFunction, MethodKind, MethodRegistry,
};
use crate::intellisense::type_provider::TypesProvider;
use crate::lexer::codes::is_token_type;
use crate::lexer::{
    ArithmeticOperator, Bracket, ComparisonOperator, Identifier, LogicalOperator, Operator,
    QuotationMark, TemplateString, Token, TokenKind,
};
use crate::variable::VariableType;

use super::literals::NodeTable;
use super::operators::Operators;
use super::{
    LabelResolver, Local, Parsed, Slot, SlotRole, SlotState, Span, ValueOption, VariableTypeSlot,
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

pub(crate) struct Classifier<'p, 'a> {
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

impl Item {
    fn collect(tokens: &[Token]) -> Vec<Self> {
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
}

impl Kind {
    fn is_operand_end(self) -> bool {
        matches!(
            self,
            Kind::Str { .. }
                | Kind::TemplateClose
                | Kind::Ident
                | Kind::Ref(_)
                | Kind::Number
                | Kind::Bool
                | Kind::Close(_)
        )
    }

    fn is_clause_boundary(self) -> bool {
        matches!(
            self,
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

    fn is_word(self) -> bool {
        matches!(
            self,
            Kind::Ident
                | Kind::Ref(_)
                | Kind::Number
                | Kind::Bool
                | Kind::Op(Operator::Logical(
                    LogicalOperator::And | LogicalOperator::Or | LogicalOperator::Not
                ))
                | Kind::Op(Operator::Comparison(
                    ComparisonOperator::In | ComparisonOperator::NotIn
                ))
        )
    }

    fn is_literal_start(self) -> bool {
        matches!(
            self,
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
}

impl<'p, 'a> Classifier<'p, 'a> {
    pub(crate) fn classify(
        parsed: &'p Parsed<'a>,
        table: &'p NodeTable<'a>,
        pos: u32,
        unary: bool,
        role: SlotRole,
        expected: Option<&'p VariableType>,
        labels: Option<&'p LabelResolver>,
    ) -> Slot {
        let (role, expected) = Self::effective_role(parsed.source, pos, role, expected);
        Self::new(parsed, table, unary, role, expected, labels).classify_at(pos)
    }

    pub(crate) fn fallback(
        source: &'p str,
        pos: u32,
        unary: bool,
        role: SlotRole,
        scope: &'p VariableType,
        expected: Option<&'p VariableType>,
        labels: Option<&'p LabelResolver>,
    ) -> Slot {
        let (role, expected) = Self::effective_role(source, pos, role, expected);
        let classifier = Self {
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
        classifier.classify_at(pos)
    }

    pub(crate) fn closure_locals(
        parsed: &'p Parsed<'a>,
        table: &'p NodeTable<'a>,
        pos: u32,
    ) -> Vec<(Rc<str>, VariableType)> {
        let classifier = Self::new(parsed, table, false, SlotRole::Condition, None, None);
        let limit = match classifier.string_at(pos) {
            Some((idx, ..)) => idx,
            None => match classifier.word_at(pos) {
                Some(w) => classifier.not_in_head(w),
                None => classifier.operand_limit(pos),
            },
        };
        classifier.locals(limit)
    }

    pub(crate) fn expected_at(&self, start: u32) -> Option<VariableType> {
        let idx = self.items.iter().position(|it| it.span.0 == start)?;
        if let Some(right) = self.comparison_after(idx) {
            let end = self.run_end(right)?;
            return self
                .operand_type(right, end)
                .and_then(VariableTypeSlot::declared);
        }
        let slot = self.at(start, idx);
        let method_arg = slot.state == SlotState::Argument
            && self
                .frames(idx)
                .last()
                .is_some_and(|f| matches!(f.kind, FrameKind::Call { method: true, .. }));
        if method_arg {
            return None;
        }
        slot.expected
    }

    fn effective_role(
        source: &str,
        pos: u32,
        role: SlotRole,
        expected: Option<&'p VariableType>,
    ) -> (SlotRole, Option<&'p VariableType>) {
        if role != SlotRole::Path
            || Self::is_path_prefix(source[..pos as usize].trim_start().as_bytes())
        {
            return (role, expected);
        }
        (SlotRole::Value, None)
    }

    fn is_path_prefix(bytes: &[u8]) -> bool {
        bytes.iter().all(|b| b.is_ascii_whitespace())
            || bytes.first().is_none_or(|b| Self::is_ident_byte(*b))
                && bytes.iter().all(|b| Self::is_ident_byte(*b) || *b == b'.')
    }

    fn is_bool(t: &VariableType) -> bool {
        matches!(t.unwrap_nullable().0, VariableType::Bool)
    }

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
        is_token_type!(b as char, "alphanumeric")
    }

    pub(crate) fn new(
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
            items: Item::collect(parsed.tokens),
            unary,
            role,
            scope: &parsed.scope,
            expected,
            labels,
        }
    }

    fn classify_at(&self, pos: u32) -> Slot {
        let mut slot = self.run(pos);
        slot.can_chain = matches!(self.role, SlotRole::Condition | SlotRole::Unary)
            && !self.frames(self.operand_limit(pos)).iter().any(|f| {
                matches!(
                    f.kind,
                    FrameKind::Call { .. }
                        | FrameKind::List { .. }
                        | FrameKind::Index
                        | FrameKind::Object
                )
            });
        if matches!(
            slot.state,
            SlotState::Value | SlotState::Argument | SlotState::ListElement
        ) {
            let mut expected = slot.expected.as_ref();
            let mut arrays = 0;
            while let Some(t) = expected {
                match t {
                    VariableType::Nullable(inner) => expected = Some(inner),
                    VariableType::Array(inner) => {
                        arrays += 1;
                        expected = Some(inner);
                    }
                    _ => break,
                }
            }
            if arrays > 0 {
                for option in &mut slot.options {
                    if !option.is_null() {
                        if let Some(source) = &mut option.source {
                            *source =
                                format!("{}{}{}", "[".repeat(arrays), source, "]".repeat(arrays));
                        }
                    }
                }
            }
        }
        slot
    }

    fn run(&self, pos: u32) -> Slot {
        if self.role == SlotRole::Path {
            return self.path_slot(pos);
        }
        if self.in_decimal(pos) {
            let mut slot = Slot::new(SlotState::Value, (pos, pos));
            slot.suppress_completions = true;
            return slot;
        }

        let limit = self.operand_limit(pos);
        if let Some(frame) = self.frames(limit).last() {
            if let FrameKind::Call {
                name,
                method: false,
            } = frame.kind
            {
                if frame.commas == 0
                    && self.is_closure(name)
                    && self
                        .collection_end(frame)
                        .and_then(|end| self.items.get(end + 1))
                        .is_some_and(|item| self.is_as(item) && item.span.1 < pos)
                {
                    let span = self
                        .word_at(pos)
                        .map(|i| self.items[i].span)
                        .unwrap_or((pos, pos));
                    let mut slot = Slot::new(SlotState::Start, span);
                    slot.suppress_completions = true;
                    return slot;
                }
            }
        }

        if let Some((idx, quote, replace)) = self.string_at(pos) {
            let item = &self.items[idx];
            let body = if quote == '`' {
                self.text((item.span.1, pos)).to_lowercase()
            } else {
                self.text((item.body.0, pos.min(item.body.1)))
                    .to_lowercase()
            };
            let mut slot = self.at(item.span.0, idx);
            slot.locals = self.slot_locals(idx);
            if matches!(slot.state, SlotState::UnaryStart | SlotState::ListElement)
                && matches!(item.kind, Kind::Str { open: false, .. })
            {
                self.unlist(&mut slot, self.text(item.body));
            }
            slot.options.retain(|o| !o.is_null());
            slot.operators.clear();
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

        if let Some(slot) = self
            .items
            .iter()
            .position(|it| it.span.1 == pos)
            .and_then(|p| self.operator_prefix(p))
        {
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
            None => (pos, self.operand_limit(pos), (pos, pos)),
        };
        let inside = word.is_none() && self.items.get(limit).is_some_and(|it| it.span.0 < pos);
        let mut slot = self.at(eff, limit);
        slot.replace_span = replace;
        slot.locals = self.slot_locals(limit);
        if inside {
            slot.auto_open = false;
        }
        let operand_due = matches!(
            slot.state,
            SlotState::Start | SlotState::Argument | SlotState::Closure
        );
        if operand_due && word.is_none() && !inside {
            slot.auto_open = true;
        }
        slot
    }
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

    fn in_decimal(&self, pos: u32) -> bool {
        let before = self.text((0, pos));
        let fraction = before.trim_end_matches(|c: char| c.is_ascii_digit());
        let Some(integer) = fraction.strip_suffix('.') else {
            return false;
        };
        let head = integer.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_');
        head.len() < integer.len()
            && !head.ends_with(|c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '.'))
            && self.string_at(pos).is_none()
    }

    fn text(&self, span: Span) -> &str {
        self.source
            .get(span.0 as usize..span.1 as usize)
            .unwrap_or_default()
    }

    fn path_slot(&self, pos: u32) -> Slot {
        let bytes = self.source.as_bytes();
        let mut start = pos as usize;
        while start > 0 && Self::is_ident_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = pos as usize;
        while end < bytes.len() && Self::is_ident_byte(bytes[end]) {
            end += 1;
        }
        let mut slot = Slot::new(SlotState::Path, (start as u32, end as u32));
        slot.auto_open = self.source.trim().is_empty();
        if start > 0 && bytes[start - 1] == b'.' {
            if let Some(dot) = self
                .items
                .iter()
                .position(|i| i.span.0 == (start - 1) as u32)
            {
                slot.operand = self.member(dot).operand;
            }
        }
        slot
    }
    fn not_in_head(&self, w: usize) -> usize {
        let is_in = self.items[w].kind == Kind::Op(Operator::Comparison(ComparisonOperator::In))
            || (self.items[w].kind == Kind::Ident && self.text(self.items[w].span) == "i");
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
            .and_then(|s| ValueOption::for_type(s, self.labels))
            .unwrap_or_default();
        slot.options.retain(|o| !slot.listed.contains(&o.value));
    }
    fn operand_limit(&self, pos: u32) -> usize {
        self.items
            .iter()
            .position(|it| it.span.1 > pos || (it.span.0 == it.span.1 && it.span.0 >= pos))
            .unwrap_or(self.items.len())
    }

    fn slot_locals(&self, limit: usize) -> Vec<Local> {
        self.locals(limit)
            .into_iter()
            .map(|(name, kind)| Local {
                name: name.to_string(),
                kind,
            })
            .collect()
    }
    fn locals(&self, limit: usize) -> Vec<(Rc<str>, VariableType)> {
        let frames = self.frames(limit);
        let mut out: Vec<(Rc<str>, VariableType)> = Vec::new();
        for frame in frames.iter().rev() {
            let FrameKind::Call {
                name,
                method: false,
            } = frame.kind
            else {
                continue;
            };
            if !self.is_closure(name) || frame.commas == 0 {
                continue;
            }
            let name: Rc<str> = self
                .collection_end(frame)
                .and_then(|end| self.closure_alias(end))
                .map(Rc::from)
                .unwrap_or_else(|| Rc::from("#"));
            if out.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let element = self.element_type(frame).unwrap_or(VariableType::Any);
            out.push((name, element));
        }
        out
    }
    fn collection_end(&self, frame: &Frame) -> Option<usize> {
        let end = self.run_end(frame.open + 1)?;
        let mut depth = 0u32;
        for i in frame.open + 2..=end {
            match self.items[i].kind {
                Kind::Open(_) | Kind::TemplateOpen => depth += 1,
                Kind::Close(_) | Kind::TemplateClose => depth = depth.saturating_sub(1),
                _ if depth == 0
                    && self.items[i - 1].kind != Kind::Op(Operator::Dot)
                    && self.is_as(&self.items[i]) =>
                {
                    return Some(i - 1);
                }
                _ => {}
            }
        }
        Some(end)
    }

    fn closure_alias(&self, end: usize) -> Option<&str> {
        let as_word = self.items.get(end + 1)?;
        if !self.is_as(as_word) {
            return None;
        }
        let alias = self
            .items
            .get(end + 2)
            .filter(|it| it.kind == Kind::Ident)?;
        Some(self.text(alias.span))
    }

    fn word_at(&self, pos: u32) -> Option<usize> {
        self.items
            .iter()
            .position(|it| it.span.0 < pos && pos <= it.span.1 && it.kind.is_word())
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
                        Some(k) if k.is_operand_end() => FrameKind::Index,
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
                                && other.is_none_or(Kind::is_clause_boundary),
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
                    .is_some_and(|k| self.items[k].kind.is_operand_end());
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
                        let subject = self.comparison_left(p - 1);
                        return self.range_slot(None, subject);
                    }
                    let unary_interval = self.unary
                        && frames.len() == 1
                        && before.is_none_or(Kind::is_clause_boundary)
                        && Self::is_ordered(&self.subject());
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
                } else {
                    self.closure_head(frames)
                }
            }
            Kind::Op(Operator::Logical(LogicalOperator::Not)) => self.closure_head(frames),
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
                let left = self.left_operand(p).map(Self::widen);
                self.start(left)
            }
            Kind::Op(Operator::Assign)
            | Kind::Open(Bracket::LeftCurlyBracket)
            | Kind::ExprStart
            | Kind::TemplateOpen
            | Kind::TemplateText
            | Kind::ExprEnd => self.start(None),
            kind if kind.is_operand_end() => self.after_operand(p, gap, frames),
            _ => self.start(None),
        }
    }
    fn whole_expected(&self, frames: &[Frame], limit: usize) -> Option<VariableType> {
        let mut current = self.expected?.shallow_clone();
        for (i, frame) in frames.iter().enumerate() {
            let end = frames.get(i + 1).map(|f| f.open).unwrap_or(limit);
            match frame.kind {
                FrameKind::Paren if self.branch_paren(frame.open) => {}
                FrameKind::List { after_in: None, .. } => {
                    current = current.innermost().shallow_clone();
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
    fn branch_slot(&self, expected: Option<VariableType>) -> Slot {
        let enumerable = expected
            .as_ref()
            .is_some_and(|e| ValueOption::for_type(e, self.labels).is_some());
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
            .and_then(|e| ValueOption::for_type(e, self.labels))
            .unwrap_or_default();
        if matches!(expected, Some(VariableType::Nullable(_))) {
            slot.options.push(ValueOption::null());
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
        slot.options = ValueOption::for_type(&subject, self.labels).unwrap_or_default();
        slot.options.retain(|o| !slot.listed.contains(&o.value));
        if matches!(subject, VariableType::Nullable(_))
            && !slot.listed.iter().any(|v| v == ValueOption::NULL)
        {
            slot.options.push(ValueOption::null());
        }
        slot.operators = Operators::for_type(&subject, true);
        slot.auto_open = !slot.options.is_empty() || !slot.operators.is_empty();
        slot.expected = Some(subject.shallow_clone());
        slot.operand = Some(subject);
        slot
    }
    fn left_operand(&self, op: usize) -> Option<VariableType> {
        let end = op.checked_sub(1)?;
        if !self.items[end].kind.is_operand_end() {
            return None;
        }
        let start = self.run_start(end, true);
        self.operand_type(start, end)
    }
    fn operand_type(&self, start: usize, end: usize) -> Option<VariableType> {
        let t = self.type_of_run(start, end, true)?;
        let literal = start == end && matches!(self.items[start].kind, Kind::Str { .. });
        Some(if literal { Self::widen(t) } else { t })
    }

    fn comparison_left(&self, op: usize) -> Option<VariableType> {
        if self.implicit_subject(op) {
            Some(self.subject())
        } else {
            self.left_operand(op)
        }
    }

    fn is_closure(&self, name: usize) -> bool {
        matches!(
            FunctionKind::try_from(self.text(self.items[name].span)),
            Ok(FunctionKind::Closure(_))
        )
    }

    fn is_as(&self, item: &Item) -> bool {
        item.kind == Kind::Ident && self.text(item.span) == "as"
    }

    fn implicit_subject(&self, op: usize) -> bool {
        self.unary
            && op
                .checked_sub(1)
                .is_none_or(|k| !self.items[k].kind.is_operand_end())
    }
    fn operator_prefix(&self, p: usize) -> Option<Slot> {
        if !matches!(
            self.items[p].kind,
            Kind::Op(Operator::Comparison(_) | Operator::Assign | Operator::QuestionMark)
                | Kind::Op(Operator::Logical(LogicalOperator::Not))
        ) {
            return None;
        }
        let typed = self.text(self.items[p].span);
        if typed.chars().all(|c| c.is_alphabetic()) {
            return None;
        }
        let operand = self.comparison_left(p);
        let operators: Vec<&'static str> =
            Operators::for_type(operand.as_ref().unwrap_or(&VariableType::Any), false)
                .into_iter()
                .filter(|op| op.starts_with(typed))
                .collect();
        if operators.len() < 2 && operators.first().is_none_or(|op| *op == typed) {
            return None;
        }
        let mut slot = Slot::new(SlotState::Operator, self.items[p].span);
        slot.operators = operators;
        slot.operand = operand;
        slot.auto_open = true;
        Some(slot)
    }

    fn value_after(&self, p: usize, op: ComparisonOperator) -> Slot {
        let left = self.comparison_left(p);
        let expected = match (op, &left) {
            (ComparisonOperator::In | ComparisonOperator::NotIn, Some(t)) => {
                Some(VariableType::Array(Rc::new(t.shallow_clone())))
            }
            (_, l) => l.clone(),
        };
        let mut slot = self.value_slot(expected, left);
        if !matches!(op, ComparisonOperator::Equal | ComparisonOperator::NotEqual) {
            slot.options.retain(|o| !o.is_null());
        }
        slot
    }

    fn member(&self, p: usize) -> Slot {
        let mut slot = Slot::new(SlotState::Member, (0, 0));
        if let Some(end) = p
            .checked_sub(1)
            .filter(|&k| self.items[k].kind.is_operand_end())
        {
            let start = self.run_start(end, false);
            slot.operand = self.type_of_run(start, end, true);
            let head = self.at(self.items[start].span.0, start);
            if head.wanted_scalar().is_some() {
                slot.expected = head.expected;
            }
        }
        slot
    }

    fn list_subject(&self, frame: &Frame) -> Option<VariableType> {
        match frame.kind {
            FrameKind::List {
                after_in: Some(op), ..
            } => self.comparison_left(op),
            FrameKind::List { implicit: true, .. } => Some(self.subject()),
            FrameKind::List { .. } => {
                let before = frame.open.checked_sub(1).map(|k| self.items[k].kind);
                match before {
                    None => self.expected.map(|t| t.innermost().shallow_clone()),
                    Some(Kind::Op(Operator::Comparison(_))) => self.left_operand(frame.open - 1),
                    Some(Kind::Open(Bracket::LeftParenthesis)) => self
                        .closure_membership(frame.open)
                        .or_else(|| self.list_expected(frame)),
                    _ => self.list_expected(frame),
                }
            }
            _ => None,
        }
    }

    fn list_expected(&self, frame: &Frame) -> Option<VariableType> {
        let head = self.at(self.items[frame.open].span.0, frame.open);
        head.expected
            .map(|t| t.innermost().shallow_clone())
            .filter(|t| !matches!(t, VariableType::Any))
    }
    fn closure_membership(&self, open: usize) -> Option<VariableType> {
        let paren = open.checked_sub(1)?;
        let name = paren.checked_sub(1)?;
        let closure = self.items[paren].kind == Kind::Open(Bracket::LeftParenthesis)
            && self.items[name].kind == Kind::Ident
            && self.is_closure(name);
        if !closure {
            return None;
        }
        let mut i = self.matched_close(open)? + 1;
        let mut alias = None;
        let as_word = self.items.get(i).is_some_and(|it| self.is_as(it));
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
        rhs.enum_domain()
            .map(|(name, values)| VariableType::Enum(name, values))
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
        if let Some(range) = self.range_ahead(limit) {
            let partner = self
                .run_end(range + 1)
                .and_then(|end| self.type_of_run(range + 1, end, true));
            return self.range_slot(partner, subject);
        }

        let mut slot = Slot::new(SlotState::ListElement, (0, 0));
        slot.listed = self.list_listed(frame.open);
        slot.options = subject
            .as_ref()
            .and_then(|s| ValueOption::for_type(s, self.labels))
            .unwrap_or_default();
        slot.options.retain(|o| !slot.listed.contains(&o.value));
        slot.expected = subject.as_ref().map(|s| s.innermost().shallow_clone());
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
        let pick = |t: Option<VariableType>| t.filter(Self::is_ordered);
        slot.expected = pick(bound)
            .or_else(|| pick(subject))
            .or(Some(VariableType::Number));
        slot
    }

    fn range_ahead(&self, limit: usize) -> Option<usize> {
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
        None
    }

    fn closure_frame<'f>(&self, frames: &'f [Frame]) -> Option<(&'f Frame, usize)> {
        for frame in frames.iter().rev() {
            match frame.kind {
                FrameKind::Paren => continue,
                FrameKind::Call {
                    name,
                    method: false,
                } => {
                    return (self.is_closure(name) && frame.commas >= 1).then_some((frame, name));
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
        let end = self.collection_end(frame)?;
        let collection = self.type_of_run(frame.open + 1, end, false)?;
        collection.iterator().map(|t| t.as_ref().shallow_clone())
    }

    fn closure_head(&self, frames: &[Frame]) -> Slot {
        let Some((frame, name)) = self.closure_frame(frames) else {
            return self.start(Some(VariableType::Bool));
        };
        let element = self.element_type(frame);
        let mut slot = Slot::new(SlotState::Closure, (0, 0));
        slot.options = element
            .as_ref()
            .and_then(|e| ValueOption::for_type(e, self.labels))
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
                .filter(|&k| self.items[k].kind.is_operand_end())
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
            .and_then(|e| ValueOption::for_type(e, self.labels))
            .unwrap_or_default();
        slot
    }
    fn is_field_run(&self, start: usize, end: usize) -> bool {
        self.items[start..=end].iter().all(|it| {
            matches!(
                it.kind,
                Kind::Ident | Kind::Ref(_) | Kind::Op(Operator::Dot)
            )
        })
    }

    fn after_operand(&self, p: usize, gap: bool, frames: &[Frame]) -> Slot {
        let start = self.run_start(p, true);
        let before = start.checked_sub(1).map(|k| self.items[k].kind);
        let operand = self.type_of_run(start, p, true);
        let at_clause = before.is_none_or(Kind::is_clause_boundary);
        let literal_clause = self.unary
            && frames.is_empty()
            && at_clause
            && self.items[start].kind.is_literal_start();
        let bool_clause = at_clause && operand.as_ref().is_some_and(Self::is_bool);
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
            slot.operators = Operators::LOGICAL.to_vec();
            let nullable = !after_comparison && matches!(operand, Some(VariableType::Nullable(_)));
            if nullable {
                slot.operators
                    .extend(Operators::nullable_extras(self.unary));
            } else if bool_clause
                && !after_comparison
                && !matches!(
                    before,
                    Some(Kind::Op(Operator::Logical(LogicalOperator::Not)))
                )
                && self.is_field_run(start, p)
            {
                slot.operators.extend(Operators::EQUALITY);
            }
            slot.auto_open = gap;
            return slot;
        }

        let mut slot = Slot::new(SlotState::Operator, (0, 0));
        slot.operators = Operators::for_type(operand.as_ref().unwrap_or(&VariableType::Any), false);
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
                .is_some_and(|k| self.items[k].kind.is_operand_end());
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

        let root = parsed.reparse(self.items[start].first, self.items[end].last)?;
        let pointer = if with_pointer {
            self.pointer_type(start)
        } else {
            None
        };
        let types =
            TypesProvider::generate(root, parsed.intellisense_scope(pointer), parsed.strict);
        types.get_type(root).map(|t| t.kind.clone())
    }

    fn pointer_type(&self, at: usize) -> Option<VariableType> {
        let frames = self.frames(at);
        let (frame, _) = self.closure_frame(&frames)?;
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
                Kind::Ref(Identifier::Null) if depth == 0 => {
                    listed.push(ValueOption::NULL.to_string())
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
                                k.is_clause_boundary()
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
                Kind::Str { open: false, .. } | Kind::Ref(Identifier::Null) => {
                    let clause_value = depth == 0
                        && before.is_none_or(|k| {
                            k.is_clause_boundary()
                                || k == Kind::Op(Operator::Comparison(ComparisonOperator::Equal))
                        });
                    if clause_value || (depth == 1 && list_open) {
                        listed.push(if it.kind == Kind::Ref(Identifier::Null) {
                            ValueOption::NULL.to_string()
                        } else {
                            self.text(it.body).to_string()
                        });
                    }
                }
                _ => {}
            }
            before = Some(it.kind);
        }
        listed
    }
}
