use std::rc::Rc;

use crate::functions::{ClosureFunction, FunctionKind};
use crate::intellisense::type_provider::TypesProvider;
use crate::intellisense::{AstMetadata, NlLabelResolver};
use crate::lexer::{Bracket, ComparisonOperator, Operator};
use crate::nl::token::{
    EditHint, EnumOption, NlToken, NlTokenKind, OpChoice, OpSym, TypeTag, WordSym,
};
use crate::parser::Node;
use crate::variable::VariableType;

struct AliasScope {
    name: Option<Box<str>>,
    elide: bool,
}

pub(crate) struct Projector<'a> {
    source: &'a str,
    types: &'a TypesProvider,
    metadata: &'a AstMetadata,
    unary: bool,
    labels: Option<NlLabelResolver>,
    aliases: Vec<AliasScope>,
    pending_elide: bool,
    pending_implied: bool,
    enums: Vec<Vec<EnumOption>>,
    out: Vec<NlToken>,
}

impl<'a> Projector<'a> {
    pub(crate) fn new(
        source: &'a str,
        types: &'a TypesProvider,
        metadata: &'a AstMetadata,
        unary: bool,
        labels: Option<NlLabelResolver>,
    ) -> Self {
        Self {
            source,
            types,
            metadata,
            unary,
            labels,
            aliases: Vec::new(),
            pending_elide: false,
            pending_implied: false,
            enums: Vec::new(),
            out: Vec::new(),
        }
    }

    pub(crate) fn run(
        mut self,
        root: &Node,
        expected: Option<VariableType>,
    ) -> (Vec<NlToken>, Vec<Vec<EnumOption>>) {
        self.project(root, expected);
        (self.out, self.enums)
    }

    fn project(&mut self, node: &Node, expected: Option<VariableType>) {
        let span = self.span_of(node);

        match node {
            Node::Null => self.push(NlTokenKind::Null, span),
            Node::Bool(value) => self.push(NlTokenKind::Bool { value: *value }, span),
            Node::Number(value) => self.push(
                NlTokenKind::Number {
                    value: value.normalize().to_string().into_boxed_str(),
                },
                span,
            ),
            Node::String(value) => {
                let hint = match Self::enum_values(expected.as_ref()) {
                    Some((name, values)) => Some(EditHint::Select {
                        options: self.intern_enum(name.as_deref(), &values),
                    }),
                    None if Self::expects_date(expected.as_ref()) => Some(EditHint::DatePicker),
                    None => None,
                };
                self.push_hint(
                    NlTokenKind::Str {
                        value: Box::from(*value),
                    },
                    span,
                    hint,
                )
            }

            Node::TemplateString(parts) => {
                self.push(NlTokenKind::TemplateOpen, (span.0, span.0));
                for part in parts.iter() {
                    match part {
                        Node::String(value) => self.push(
                            NlTokenKind::TemplateText {
                                value: Box::from(*value),
                            },
                            self.span_of(part),
                        ),
                        other => self.project(other, None),
                    }
                }
                self.push(NlTokenKind::TemplateClose, (span.1, span.1));
            }

            Node::Root => self.push(NlTokenKind::Root, span),
            Node::Pointer => self.push(NlTokenKind::Element { alias: None }, span),

            Node::Identifier(name) => {
                if *name == "$" {
                    self.push(NlTokenKind::Context, span);
                } else if self
                    .aliases
                    .iter()
                    .any(|scope| scope.name.as_deref() == Some(*name))
                {
                    self.push(
                        NlTokenKind::Element {
                            alias: Some(Box::from(*name)),
                        },
                        span,
                    );
                } else {
                    let ty = self.tag_of(&self.type_of(node));
                    self.push(
                        NlTokenKind::Field {
                            path: vec![Box::from(*name)],
                            ty,
                        },
                        span,
                    );
                }
            }

            Node::Member { .. } => match Self::field_path(node) {
                Some(field) => {
                    let field = self.strip_elided_alias(field);
                    let ty = self.tag_of(&self.type_of(node));
                    self.push(NlTokenKind::Field { path: field, ty }, span);
                }
                None => self.code(span),
            },

            Node::Binary {
                left,
                operator,
                right,
            } => {
                if let Some(sym) = self.contains_sym(*operator, right) {
                    self.contains_flipped(sym, left, right);
                    return;
                }
                let (exp_left, exp_right) = if matches!(
                    operator,
                    Operator::Logical(crate::lexer::LogicalOperator::NullishCoalescing)
                ) {
                    (expected.clone(), expected)
                } else {
                    self.operand_expectations(*operator, left, right)
                };
                let context_subject = matches!(operator, Operator::Comparison(_))
                    && matches!(left, Node::Identifier(name) if *name == "$");
                if !context_subject && !self.is_elided_subject(left) {
                    self.project(left, exp_left);
                }
                if let Some(sym) = OpSym::from_operator(*operator) {
                    let op_span = (self.span_of(left).1, self.span_of(right).0);
                    let hint = self.op_hint(*operator, left);
                    let implied = context_subject || std::mem::take(&mut self.pending_implied);
                    let between = matches!(sym, OpSym::In | OpSym::NotIn)
                        && matches!(right, Node::Interval { .. });
                    self.push_hint(
                        NlTokenKind::Op {
                            sym,
                            implied,
                            between,
                        },
                        op_span,
                        hint,
                    );
                }
                self.project(right, exp_right);
            }

            Node::Unary {
                node: inner,
                operator,
            } => {
                if let Some(sym) = OpSym::from_operator(*operator) {
                    let implied = std::mem::take(&mut self.pending_implied);
                    self.push(
                        NlTokenKind::Op {
                            sym,
                            implied,
                            between: false,
                        },
                        (span.0, self.span_of(inner).0),
                    );
                }
                self.project(inner, None);
            }

            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.push(NlTokenKind::Word { sym: WordSym::If }, (span.0, span.0));
                self.project(condition, None);
                self.push(
                    NlTokenKind::Word { sym: WordSym::Then },
                    (self.span_of(condition).1, self.span_of(on_true).0),
                );
                self.project(on_true, expected.clone());
                self.push(
                    NlTokenKind::Word {
                        sym: WordSym::Otherwise,
                    },
                    (self.span_of(on_true).1, self.span_of(on_false).0),
                );
                self.project(on_false, expected);
            }

            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } => {
                self.push(
                    NlTokenKind::IntervalOpen {
                        inclusive: *left_bracket == Bracket::LeftSquareBracket,
                    },
                    (span.0, span.0),
                );
                self.project(left, None);
                self.push(
                    NlTokenKind::Word {
                        sym: WordSym::RangeAnd,
                    },
                    (self.span_of(left).1, self.span_of(right).0),
                );
                self.project(right, None);
                self.push(
                    NlTokenKind::IntervalClose {
                        inclusive: *right_bracket == Bracket::RightSquareBracket,
                    },
                    (span.1, span.1),
                );
            }

            Node::Parenthesized(inner) => {
                self.push(NlTokenKind::GroupOpen, (span.0, span.0));
                self.project(inner, expected);
                self.push(NlTokenKind::GroupClose, (span.1, span.1));
            }

            Node::Array(items) => {
                let enum_domain = Self::enum_values(expected.as_ref())
                    .filter(|_| items.iter().all(|item| matches!(item, Node::String(_))));
                if let Some((name, values)) = enum_domain {
                    let selected = items
                        .iter()
                        .filter_map(|item| match item {
                            Node::String(value) => Some(Box::from(*value)),
                            _ => None,
                        })
                        .collect();
                    let hint = EditHint::MultiSelect {
                        options: self.intern_enum(name.as_deref(), &values),
                    };
                    self.push_hint(NlTokenKind::EnumList { selected }, span, Some(hint));
                    return;
                }
                self.push(NlTokenKind::ListOpen, (span.0, span.0));
                let item_expected = Self::item_expectation(expected.as_ref());
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        let prev = self.span_of(items[i - 1]);
                        self.push(NlTokenKind::Comma, (prev.1, self.span_of(item).0));
                    }
                    self.project(item, item_expected.clone());
                }
                self.push(NlTokenKind::ListClose, (span.1, span.1));
            }

            Node::FunctionCall { kind, arguments } => self.function_call(kind, arguments, span),

            Node::MethodCall {
                kind,
                this,
                arguments,
            } => {
                self.project(this, None);
                let this_end = self.span_of(this).1;
                self.push(
                    NlTokenKind::Method {
                        sym: kind.to_string().into_boxed_str(),
                    },
                    (this_end, span.1),
                );
                if let [only] = arguments {
                    if Self::is_simple_arg(only) {
                        self.project(only, None);
                        return;
                    }
                }
                self.group_args(arguments, this_end, span.1);
            }

            Node::Closure { body, alias } => {
                let elide = std::mem::take(&mut self.pending_elide);
                self.aliases.push(AliasScope {
                    name: alias.map(Box::from),
                    elide,
                });
                self.project(body, expected);
                self.aliases.pop();
            }

            Node::Assignments { list, output } => {
                for (i, (key, value)) in list.iter().enumerate() {
                    if i > 0 {
                        let prev_end = self.span_of(list[i - 1].1).1;
                        self.push(NlTokenKind::StmtEnd, (prev_end, self.span_of(key).0));
                    }
                    match key {
                        Node::Identifier(name) | Node::String(name) => {
                            let ty = self.tag_of(&self.type_of(value));
                            self.push(
                                NlTokenKind::Field {
                                    path: vec![Box::from(*name)],
                                    ty,
                                },
                                self.span_of(key),
                            );
                        }
                        other => self.project(other, None),
                    }
                    self.push(
                        NlTokenKind::Assign,
                        (self.span_of(key).1, self.span_of(value).0),
                    );
                    self.project(value, None);
                }
                if let Some(output) = output {
                    if let Some(last) = list.last() {
                        self.push(
                            NlTokenKind::StmtEnd,
                            (self.span_of(last.1).1, self.span_of(output).0),
                        );
                    }
                    self.project(output, expected);
                }
            }

            Node::Object(_) | Node::Slice { .. } | Node::Error { .. } => self.code(span),
        }
    }

    fn function_call(&mut self, kind: &FunctionKind, arguments: &[&Node], span: (u32, u32)) {
        let sym = kind.to_string().into_boxed_str();
        let FunctionKind::Closure(cf) = kind else {
            if Self::is_infix_predicate(&sym) && arguments.len() == 2 {
                let (left, right) = (arguments[0], arguments[1]);
                self.project(left, None);
                self.push(
                    NlTokenKind::Func {
                        sym,
                        closure: false,
                    },
                    (self.span_of(left).1, self.span_of(right).0),
                );
                let needle_expected = match self.type_of(left) {
                    haystack @ VariableType::Array(_) => Some(haystack),
                    _ => None,
                };
                self.project(right, needle_expected);
                return;
            }
            if sym.as_ref() == "d" && matches!(arguments.first(), Some(Node::String(_))) {
                if let [only] = arguments {
                    self.project(only, Some(VariableType::Date));
                    return;
                }
                self.push(
                    NlTokenKind::Func {
                        sym,
                        closure: false,
                    },
                    (span.0, span.0),
                );
                self.group_args_expected(arguments, span.0, span.1, Some(VariableType::Date));
                return;
            }
            let suppressed = self.unary && sym.as_ref() == "bool" && self.out.is_empty();
            if !suppressed {
                self.push(
                    NlTokenKind::Func {
                        sym,
                        closure: false,
                    },
                    (span.0, span.0),
                );
            }
            if let [only] = arguments {
                if suppressed || Self::is_simple_arg(only) {
                    self.project(only, None);
                    return;
                }
            }
            self.group_args(arguments, span.0, span.1);
            return;
        };

        if self.quantified_membership(*cf, arguments, span) {
            return;
        }

        let quant = Self::is_quantifier(*cf)
            && arguments.len() == 2
            && matches!(arguments[1], Node::Closure { .. });
        let name_span = if quant {
            (span.0, span.0 + sym.len() as u32)
        } else {
            (span.0, span.0)
        };
        let hint = quant.then(|| EditHint::FuncSelect {
            options: vec!["all".into(), "some".into(), "none".into()],
        });
        self.push_hint(NlTokenKind::Func { sym, closure: true }, name_span, hint);

        let alias = match arguments.get(1) {
            Some(Node::Closure { alias, .. }) => *alias,
            _ => None,
        };
        let elide = self.aliases.is_empty();
        if !elide {
            self.push(
                NlTokenKind::Element {
                    alias: alias.map(Box::from),
                },
                (span.0, span.0),
            );
            self.push(NlTokenKind::Word { sym: WordSym::In }, (span.0, span.0));
        }
        let membership = self.closure_membership_expectation(arguments, alias);
        if let Some(collection) = arguments.first() {
            self.project(collection, membership);
        }
        let leftmost = match arguments.get(1) {
            Some(Node::Closure { body, .. }) => Some(Self::leftmost_leaf(body)),
            _ => None,
        };
        let body_leads_with_subject =
            elide && leftmost.is_some_and(|leaf| Self::is_binding_leaf(leaf, alias));
        if !body_leads_with_subject {
            let sym = if elide && leftmost.is_some_and(|leaf| Self::is_binding_member(leaf, alias))
            {
                self.pending_implied = true;
                WordSym::Has
            } else {
                WordSym::Where
            };
            self.push(NlTokenKind::Word { sym }, (span.1, span.1));
        }
        if let Some(closure) = arguments.get(1) {
            self.pending_elide = elide;
            self.project(closure, None);
        }
    }

    fn is_infix_predicate(sym: &str) -> bool {
        matches!(
            sym,
            "contains" | "startsWith" | "endsWith" | "matches" | "fuzzyMatch"
        )
    }

    fn is_quantifier(cf: ClosureFunction) -> bool {
        matches!(
            cf,
            ClosureFunction::All | ClosureFunction::Some | ClosureFunction::None
        )
    }

    fn is_array(ty: &VariableType) -> bool {
        match ty {
            VariableType::Array(_) => true,
            VariableType::Nullable(inner) => Self::is_array(inner),
            _ => false,
        }
    }

    fn contains_sym(&self, operator: Operator, right: &Node) -> Option<OpSym> {
        let Operator::Comparison(cmp) = operator else {
            return None;
        };
        let sym = match cmp {
            ComparisonOperator::In => OpSym::Contains,
            ComparisonOperator::NotIn => OpSym::NotContains,
            _ => return None,
        };
        if matches!(right, Node::Array(_) | Node::Interval { .. }) {
            return None;
        }
        Self::is_array(&self.type_of(right)).then_some(sym)
    }

    fn contains_flipped(&mut self, sym: OpSym, left: &Node, right: &Node) {
        let context_subject = matches!(right, Node::Identifier(name) if *name == "$");
        if !context_subject && !self.is_elided_subject(right) {
            self.project(right, None);
        }
        let op_span = (self.span_of(left).1, self.span_of(right).0);
        let hint = EditHint::OpSelect {
            options: vec![
                OpChoice::from(OpSym::Contains),
                OpChoice::from(OpSym::NotContains),
            ],
        };
        let implied = context_subject || std::mem::take(&mut self.pending_implied);
        self.push_hint(
            NlTokenKind::Op {
                sym,
                implied,
                between: false,
            },
            op_span,
            Some(hint),
        );
        let expected = Some(self.type_of(right));
        self.project(left, expected);
    }

    fn quantified_membership(
        &mut self,
        cf: ClosureFunction,
        arguments: &[&Node],
        span: (u32, u32),
    ) -> bool {
        if !Self::is_quantifier(cf) {
            return false;
        }
        let [collection, closure] = arguments else {
            return false;
        };
        let Node::Closure { body, alias } = closure else {
            return false;
        };
        let mut node: &Node = body;
        while let Node::Parenthesized(inner) = node {
            node = inner;
        }
        let Node::Binary {
            left,
            operator: Operator::Comparison(cmp),
            right,
        } = node
        else {
            return false;
        };
        let negated = match cmp {
            ComparisonOperator::In => false,
            ComparisonOperator::NotIn => true,
            _ => return false,
        };
        if !Self::is_binding_leaf(left, *alias) {
            return false;
        }
        if !Self::membership_operand(right, *alias) {
            return false;
        }
        let flipped = matches!(collection, Node::Array(_)) && !matches!(right, Node::Array(_));
        let (subject, list): (&Node, &Node) = if flipped {
            (right, collection)
        } else {
            (collection, right)
        };
        let subject_ty = self.type_of(subject);
        if !matches!(subject_ty, VariableType::Any) && !Self::is_array(&subject_ty) {
            return false;
        }
        let sym = match (flipped, cf, negated) {
            (false, ClosureFunction::Some, false) => OpSym::ContainsAny,
            (false, ClosureFunction::None, false) => OpSym::ContainsNone,
            (false, ClosureFunction::All, false) => OpSym::ContainsOnly,
            (false, ClosureFunction::All, true) => OpSym::ContainsNone,
            (false, ClosureFunction::None, true) => OpSym::ContainsOnly,
            (true, ClosureFunction::Some, false) => OpSym::ContainsAny,
            (true, ClosureFunction::All, false) => OpSym::ContainsAll,
            (true, ClosureFunction::None, false) => OpSym::ContainsNone,
            (true, ClosureFunction::All, true) => OpSym::ContainsNone,
            (true, ClosureFunction::None, true) => OpSym::ContainsAll,
            _ => return false,
        };
        let implied = matches!(subject, Node::Identifier(name) if *name == "$");
        if !implied {
            self.project(subject, None);
        }
        let hint = EditHint::QuantSelect {
            options: vec![
                OpSym::ContainsAny,
                OpSym::ContainsAll,
                OpSym::ContainsNone,
                OpSym::ContainsOnly,
            ],
            subject: self.slice(self.span_of(subject)),
            list: self.slice(self.span_of(list)),
        };
        self.push_hint(
            NlTokenKind::Op {
                sym,
                implied,
                between: false,
            },
            span,
            Some(hint),
        );
        self.project(list, Some(subject_ty));
        true
    }

    fn membership_operand(node: &Node, alias: Option<&str>) -> bool {
        match node {
            Node::Array(items) => items
                .iter()
                .all(|item| matches!(item, Node::String(_) | Node::Number(_) | Node::Bool(_))),
            Node::Identifier(name) => alias != Some(*name),
            Node::Member { .. } => match Self::field_path(node) {
                Some(path) => match (path.first(), alias) {
                    (Some(head), Some(bound)) => head.as_ref() != bound && head.as_ref() != "#",
                    (Some(head), None) => head.as_ref() != "#",
                    _ => false,
                },
                None => false,
            },
            _ => false,
        }
    }

    fn slice(&self, span: (u32, u32)) -> Box<str> {
        Box::from(
            self.source
                .get(span.0 as usize..span.1 as usize)
                .unwrap_or_default(),
        )
    }

    fn is_simple_arg(node: &Node) -> bool {
        match node {
            Node::Identifier(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::Bool(_)
            | Node::Pointer
            | Node::Array(_) => true,
            Node::Member { .. } => Self::field_path(node).is_some(),
            _ => false,
        }
    }

    fn closure_membership_expectation(
        &self,
        arguments: &[&Node],
        alias: Option<&str>,
    ) -> Option<VariableType> {
        let Some(Node::Closure { body, .. }) = arguments.get(1) else {
            return None;
        };
        let mut node: &Node = body;
        while let Node::Parenthesized(inner) = node {
            node = inner;
        }
        let Node::Binary {
            left,
            operator: Operator::Comparison(cmp),
            right,
        } = node
        else {
            return None;
        };
        if !matches!(cmp, ComparisonOperator::In | ComparisonOperator::NotIn) {
            return None;
        }
        if !Self::is_binding_leaf(left, alias) {
            return None;
        }
        let rhs = self.type_of(right);
        Self::enum_values(Some(&rhs)).map(|(name, values)| VariableType::Enum(name, values))
    }

    fn leftmost_leaf<'n>(body: &'n Node<'n>) -> &'n Node<'n> {
        let mut node = body;
        loop {
            match node {
                Node::Binary { left, .. } => node = left,
                Node::Parenthesized(inner) => node = inner,
                other => return other,
            }
        }
    }

    fn is_binding_leaf(leaf: &Node, alias: Option<&str>) -> bool {
        match leaf {
            Node::Pointer => alias.is_none(),
            Node::Identifier(name) => alias == Some(*name),
            _ => false,
        }
    }

    fn is_binding_member(leaf: &Node, alias: Option<&str>) -> bool {
        match leaf {
            Node::Member { .. } => match Self::field_path(leaf) {
                Some(field) => match (field.first(), alias) {
                    (Some(head), Some(alias)) => head.as_ref() == alias,
                    (Some(head), None) => head.as_ref() == "#",
                    _ => false,
                },
                None => false,
            },
            _ => false,
        }
    }

    fn alias_elided(&self, name: Option<&str>) -> bool {
        match name {
            None => self
                .aliases
                .last()
                .is_some_and(|scope| scope.elide && scope.name.is_none()),
            Some(name) => self
                .aliases
                .iter()
                .rev()
                .find(|scope| scope.name.as_deref() == Some(name))
                .is_some_and(|scope| scope.elide),
        }
    }

    fn is_elided_subject(&self, node: &Node) -> bool {
        match node {
            Node::Pointer => self.alias_elided(None),
            Node::Identifier(name) => self.alias_elided(Some(name)),
            _ => false,
        }
    }

    fn strip_elided_alias(&self, field: Vec<Box<str>>) -> Vec<Box<str>> {
        if field.len() < 2 {
            return field;
        }
        let head = field[0].as_ref();
        let elided = self.alias_elided((head != "#").then_some(head));
        if elided {
            field.into_iter().skip(1).collect()
        } else {
            field
        }
    }

    fn group_args(&mut self, arguments: &[&Node], open: u32, close: u32) {
        self.group_args_expected(arguments, open, close, None);
    }

    fn group_args_expected(
        &mut self,
        arguments: &[&Node],
        open: u32,
        close: u32,
        mut first_expected: Option<VariableType>,
    ) {
        self.push(NlTokenKind::GroupOpen, (open, open));
        for (i, arg) in arguments.iter().enumerate() {
            if i > 0 {
                let prev = self.span_of(arguments[i - 1]);
                self.push(NlTokenKind::Comma, (prev.1, self.span_of(arg).0));
            }
            self.project(arg, first_expected.take());
        }
        self.push(NlTokenKind::GroupClose, (close, close));
    }

    fn op_hint(&self, operator: Operator, left: &Node) -> Option<EditHint> {
        use crate::lexer::ComparisonOperator as Cmp;
        use crate::lexer::LogicalOperator as Log;
        let options = match operator {
            Operator::Comparison(Cmp::In | Cmp::NotIn) => vec![OpSym::In, OpSym::NotIn],
            Operator::Comparison(_) => {
                if Self::is_ordered(&self.type_of(left)) {
                    vec![
                        OpSym::Gt,
                        OpSym::Gte,
                        OpSym::Lt,
                        OpSym::Lte,
                        OpSym::Eq,
                        OpSym::Ne,
                    ]
                } else {
                    vec![OpSym::Eq, OpSym::Ne]
                }
            }
            Operator::Logical(Log::And | Log::Or) => vec![OpSym::And, OpSym::Or],
            Operator::Arithmetic(_) => vec![OpSym::Add, OpSym::Sub, OpSym::Mul, OpSym::Div],
            _ => return None,
        };
        Some(EditHint::OpSelect {
            options: options.into_iter().map(OpChoice::from).collect(),
        })
    }

    fn is_ordered(ty: &VariableType) -> bool {
        match ty {
            VariableType::Number | VariableType::Date | VariableType::Any => true,
            VariableType::Nullable(inner) => Self::is_ordered(inner),
            _ => false,
        }
    }

    fn operand_expectations(
        &self,
        operator: Operator,
        left: &Node,
        right: &Node,
    ) -> (Option<VariableType>, Option<VariableType>) {
        let Operator::Comparison(_) = operator else {
            return (None, None);
        };
        (Some(self.type_of(right)), Some(self.type_of(left)))
    }

    fn code(&mut self, span: (u32, u32)) {
        let source = self
            .source
            .get(span.0 as usize..span.1 as usize)
            .unwrap_or_default();
        self.push(
            NlTokenKind::Code {
                source: Box::from(source),
            },
            span,
        );
    }

    fn push(&mut self, token: NlTokenKind, span: (u32, u32)) {
        self.push_hint(token, span, None);
    }

    fn push_hint(&mut self, token: NlTokenKind, span: (u32, u32), hint: Option<EditHint>) {
        self.out.push(NlToken { token, span, hint });
    }

    fn span_of(&self, node: &Node) -> (u32, u32) {
        let addr = node as *const Node as usize;
        node.span()
            .or_else(|| self.metadata.get(&addr).map(|m| m.span))
            .unwrap_or_default()
    }

    fn type_of(&self, node: &Node) -> VariableType {
        self.types
            .get_type(node)
            .map(|t| t.kind.clone())
            .unwrap_or(VariableType::Any)
    }

    fn tag_of(&mut self, ty: &VariableType) -> TypeTag {
        match ty {
            VariableType::Number => TypeTag::Number,
            VariableType::String | VariableType::Const(_) => TypeTag::String,
            VariableType::Bool => TypeTag::Bool,
            VariableType::Date => TypeTag::Date,
            VariableType::Interval => TypeTag::Interval,
            VariableType::Object(_) => TypeTag::Object,
            VariableType::Null => TypeTag::Null,
            VariableType::Any => TypeTag::Unknown,
            VariableType::Enum(name, values) => TypeTag::Enum {
                index: self.intern_enum(name.as_deref(), values),
            },
            VariableType::Array(inner) => TypeTag::Array {
                items: Box::new(self.tag_of(inner)),
            },
            VariableType::Nullable(inner) => self.tag_of(inner),
        }
    }

    fn intern_enum(&mut self, name: Option<&str>, values: &[Rc<str>]) -> u32 {
        let options = crate::nl::enum_options(name, values, self.labels.as_ref());
        let existing = self.enums.iter().position(|e| *e == options);
        if let Some(index) = existing {
            return index as u32;
        }
        self.enums.push(options);
        (self.enums.len() - 1) as u32
    }

    fn enum_values(ty: Option<&VariableType>) -> Option<(Option<Rc<str>>, Vec<Rc<str>>)> {
        match ty? {
            VariableType::Enum(name, values) => Some((name.clone(), values.clone())),
            VariableType::Nullable(inner) | VariableType::Array(inner) => {
                Self::enum_values(Some(inner))
            }
            _ => None,
        }
    }

    fn expects_date(ty: Option<&VariableType>) -> bool {
        match ty {
            Some(VariableType::Date) => true,
            Some(VariableType::Nullable(inner)) => Self::expects_date(Some(inner)),
            _ => false,
        }
    }

    fn item_expectation(ty: Option<&VariableType>) -> Option<VariableType> {
        match ty? {
            VariableType::Array(inner) | VariableType::Nullable(inner) => {
                Self::item_expectation(Some(inner))
            }
            other => Some(other.shallow_clone()),
        }
    }

    fn field_path(node: &Node) -> Option<Vec<Box<str>>> {
        match node {
            Node::Identifier(name) => Some(vec![Box::from(*name)]),
            Node::Pointer => Some(vec![Box::from("#")]),
            Node::Root => Some(vec![Box::from("$root")]),
            Node::Member { node, property } => {
                let mut base = Self::field_path(node)?;
                match property {
                    Node::String(s) => base.push(Box::from(*s)),
                    Node::Root => base.push(Box::from("$root")),
                    Node::Number(n) if n.is_integer() && !n.is_sign_negative() => {
                        let last = base.pop()?;
                        base.push(Box::from(format!("{last}[{n}]").as_str()));
                    }
                    _ => return None,
                }
                Some(base)
            }
            _ => None,
        }
    }
}
