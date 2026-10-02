use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use crate::functions::{FunctionKind, InternalFunction};
use crate::intellisense::IntelliSense;
use crate::lexer::{ArithmeticOperator, Bracket, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use rust_decimal::Decimal;

use super::print::DateDay;
use super::value_set::{Bound, Interval, NumberSet, ValueSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CellConstraint {
    Any,
    Known(ValueSet),
    Opaque(Rc<str>),
}

impl CellConstraint {
    pub fn parse(
        is: &mut IntelliSense,
        source: &str,
        unary: bool,
        analyzable: bool,
        dated: bool,
    ) -> Self {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return CellConstraint::Any;
        }
        let parsed = is.with_ast(trimmed, unary, |node, _| {
            let node = Truth::unwrap(node);
            let truth = if analyzable && unary {
                Truth::of(
                    node,
                    &Scope {
                        subject: &["$"],
                        dated,
                    },
                )
            } else {
                None
            };
            match truth {
                Some(truth) => Ok(truth.t),
                None => Err((Self::is_random(node), format!("{node:?}"))),
            }
        });
        match parsed {
            Some(Ok(set)) => CellConstraint::Known(set),
            Some(Err((random, key))) => CellConstraint::Opaque(Self::atom_key(random, key)),
            None => CellConstraint::Opaque(Self::atom_key(false, format!("src:{trimmed}"))),
        }
    }

    fn is_random(node: &Node) -> bool {
        let random = Cell::new(false);
        node.walk(|n| {
            if let Node::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Rand),
                ..
            } = n
            {
                random.set(true);
            }
        });
        random.get()
    }

    fn atom_key(random: bool, key: String) -> Rc<str> {
        if random {
            static UNIQUE: AtomicUsize = AtomicUsize::new(0);
            return Rc::from(format!(
                "unique:{}",
                UNIQUE.fetch_add(1, AtomicOrdering::Relaxed)
            ));
        }
        Rc::from(key)
    }

    pub fn known_set(&self) -> Option<ValueSet> {
        match self {
            CellConstraint::Any => Some(ValueSet::all()),
            CellConstraint::Known(set) => Some(set.clone()),
            CellConstraint::Opaque(_) => None,
        }
    }
}

pub struct FieldPath;

impl FieldPath {
    pub fn of(is: &mut IntelliSense, source: &str) -> Option<Vec<Rc<str>>> {
        is.with_ast(source.trim(), false, |node, _| {
            Truth::path(node).map(|path| path.into_iter().map(Rc::from).collect())
        })
        .flatten()
    }
}

pub struct Condition;

impl Condition {
    pub fn holds(is: &mut IntelliSense, source: &str) -> Vec<(Arc<str>, ValueSet)> {
        is.with_ast(source.trim(), false, |node, _| {
            let mut conjuncts = Vec::new();
            Truth::conjuncts(node, &mut conjuncts);
            let mut out: Vec<(Arc<str>, ValueSet)> = Vec::new();
            for conjunct in conjuncts {
                let Some(subject) = Truth::subject(conjunct) else {
                    continue;
                };
                if subject.first() == Some(&"$") {
                    continue;
                }
                let scope = Scope {
                    subject: &subject,
                    dated: false,
                };
                let Some(truth) = Truth::of(conjunct, &scope) else {
                    continue;
                };
                let key: Arc<str> = Arc::from(subject.join("."));
                match out.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, set)) => *set = set.intersect(&truth.t),
                    None => out.push((key, truth.t)),
                }
            }
            out
        })
        .unwrap_or_default()
    }

    pub fn fails(is: &mut IntelliSense, source: &str) -> Option<(Arc<str>, ValueSet)> {
        let mut holds = Self::holds(is, source);
        let single = is
            .with_ast(source.trim(), false, |node, _| {
                let mut conjuncts = Vec::new();
                Truth::conjuncts(node, &mut conjuncts);
                conjuncts.len() == 1
            })
            .unwrap_or(false);
        match (single, holds.len()) {
            (true, 1) => holds.pop().map(|(path, set)| (path, set.complement())),
            _ => None,
        }
    }
}

pub struct Scope<'s> {
    subject: &'s [&'s str],
    dated: bool,
}

struct Truth {
    t: ValueSet,
    f: ValueSet,
}

impl Truth {
    fn unwrap<'a, 'n>(mut node: &'a Node<'n>) -> &'a Node<'n> {
        while let Node::Parenthesized(inner) = node {
            node = inner;
        }
        node
    }

    fn chain<'a, 'n>(node: &'a Node<'n>, op: LogicalOperator) -> Vec<&'a Node<'n>> {
        let mut operands = Vec::new();
        let mut pending = vec![node];
        while let Some(next) = pending.pop() {
            match Self::unwrap(next) {
                Node::Binary {
                    left,
                    operator: Operator::Logical(found),
                    right,
                } if *found == op => {
                    pending.push(right);
                    pending.push(left);
                }
                other => operands.push(other),
            }
        }
        operands
    }

    fn all(node: &Node, cx: &Scope) -> Option<Truth> {
        let mut operands = Self::chain(node, LogicalOperator::And).into_iter();
        let mut acc = Self::of(operands.next()?, cx)?;
        for operand in operands {
            let b = Self::of(operand, cx)?;
            acc = Truth {
                f: acc.f.union(&acc.t.intersect(&b.f)),
                t: acc.t.intersect(&b.t),
            };
        }
        Some(acc)
    }

    fn any(node: &Node, cx: &Scope) -> Option<Truth> {
        let mut acc: Option<Truth> = None;
        let mut total: Vec<ValueSet> = Vec::new();
        for operand in Self::chain(node, LogicalOperator::Or) {
            let b = Self::of(operand, cx)?;
            if !b.t.intersects(&b.f) && b.t.union(&b.f).is_all() {
                total.push(b.t);
                continue;
            }
            if !total.is_empty() {
                acc = Some(Self::either(acc, Self::total(&total)));
                total.clear();
            }
            acc = Some(Self::either(acc, b));
        }
        if !total.is_empty() {
            acc = Some(Self::either(acc, Self::total(&total)));
        }
        acc
    }

    fn total(sets: &[ValueSet]) -> Truth {
        let t = ValueSet::union_all(sets);
        let f = t.complement();
        Truth { t, f }
    }

    fn either(acc: Option<Truth>, b: Truth) -> Truth {
        match acc {
            None => b,
            Some(a) => Truth {
                t: a.t.union(&a.f.intersect(&b.t)),
                f: a.f.intersect(&b.f),
            },
        }
    }

    fn of(node: &Node, cx: &Scope) -> Option<Truth> {
        match Self::unwrap(node) {
            Node::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Bool),
                arguments: [argument],
            } if Self::is_boolean(argument) => Self::of(argument, cx),
            Node::Unary {
                operator: Operator::Logical(LogicalOperator::Not),
                node,
            } => Self::of(node, cx).map(|inner| Truth {
                t: inner.f,
                f: inner.t,
            }),
            Node::Binary {
                operator: Operator::Logical(LogicalOperator::And),
                ..
            } => Self::all(node, cx),
            Node::Binary {
                operator: Operator::Logical(LogicalOperator::Or),
                ..
            } => Self::any(node, cx),
            Node::Binary {
                left,
                operator: Operator::Comparison(op),
                right,
            } => Self::comparison(left, *op, right, cx),
            _ => None,
        }
    }

    fn is_boolean(node: &Node) -> bool {
        match Self::unwrap(node) {
            Node::Binary {
                operator:
                    Operator::Comparison(_)
                    | Operator::Logical(LogicalOperator::And | LogicalOperator::Or),
                ..
            } => true,
            Node::Unary {
                operator: Operator::Logical(LogicalOperator::Not),
                node,
            } => Self::is_boolean(node),
            _ => false,
        }
    }

    fn is_reference(node: &Node, subject: &[&str]) -> bool {
        Self::path(node).is_some_and(|path| path == subject)
    }

    fn path<'a>(node: &Node<'a>) -> Option<Vec<&'a str>> {
        match Self::unwrap(node) {
            Node::Identifier(name) => Some(vec![*name]),
            Node::Member { node, property } => match Self::unwrap(property) {
                Node::String(key) => {
                    let mut path = Self::path(node)?;
                    path.push(key);
                    Some(path)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn subject<'a>(mut node: &Node<'a>) -> Option<Vec<&'a str>> {
        loop {
            node = match Self::unwrap(node) {
                Node::FunctionCall {
                    kind: FunctionKind::Internal(InternalFunction::Bool),
                    arguments: [argument],
                } => argument,
                Node::Unary {
                    operator: Operator::Logical(LogicalOperator::Not),
                    node,
                } => node,
                Node::Binary {
                    left,
                    operator: Operator::Logical(LogicalOperator::And | LogicalOperator::Or),
                    ..
                } => left,
                Node::Binary {
                    left,
                    operator: Operator::Comparison(_),
                    right,
                } => return Self::path(left).or_else(|| Self::path(right)),
                _ => return None,
            };
        }
    }

    fn conjuncts<'n, 'a>(node: &'n Node<'a>, out: &mut Vec<&'n Node<'a>>) {
        out.extend(Self::chain(node, LogicalOperator::And));
    }

    fn comparison(left: &Node, op: ComparisonOperator, right: &Node, cx: &Scope) -> Option<Truth> {
        use ComparisonOperator as C;
        let (literal, op) = match (
            Self::is_reference(left, cx.subject),
            Self::is_reference(right, cx.subject),
        ) {
            (true, false) => (right, op),
            (false, true) => match op {
                C::Equal | C::NotEqual => (left, op),
                C::LessThan => (left, C::GreaterThan),
                C::LessThanOrEqual => (left, C::GreaterThanOrEqual),
                C::GreaterThan => (left, C::LessThan),
                C::GreaterThanOrEqual => (left, C::LessThanOrEqual),
                C::In | C::NotIn => return None,
            },
            _ => return None,
        };
        match op {
            C::Equal => Self::equality(literal, cx),
            C::NotEqual => Self::equality(literal, cx).map(Truth::negate),
            C::In => Self::membership(literal, cx),
            C::NotIn => Self::membership(literal, cx).map(Truth::negate),
            C::LessThan | C::LessThanOrEqual | C::GreaterThan | C::GreaterThanOrEqual => {
                let x = Self::number(literal, cx)?;
                let interval = match op {
                    C::LessThan => Interval::new(Bound::Unbounded, Bound::Exclusive(x)),
                    C::LessThanOrEqual => Interval::new(Bound::Unbounded, Bound::Inclusive(x)),
                    C::GreaterThan => Interval::new(Bound::Exclusive(x), Bound::Unbounded),
                    _ => Interval::new(Bound::Inclusive(x), Bound::Unbounded),
                };
                Some(Self::numeric(NumberSet::from_intervals(vec![interval])))
            }
        }
    }

    fn negate(self) -> Truth {
        Truth {
            t: self.f,
            f: self.t,
        }
    }

    fn numeric(t: NumberSet) -> Truth {
        let f = t.complement();
        Truth {
            t: ValueSet::numbers(t),
            f: ValueSet::numbers(f),
        }
    }

    fn equality(literal: &Node, cx: &Scope) -> Option<Truth> {
        let t = Self::literal(literal, cx)?;
        let f = ValueSet::all().difference(&t);
        Some(Truth { t, f })
    }

    fn membership(right: &Node, cx: &Scope) -> Option<Truth> {
        match Self::unwrap(right) {
            Node::Array(items) => {
                let literals = items
                    .iter()
                    .map(|item| Self::literal(item, cx))
                    .collect::<Option<Vec<_>>>()?;
                let t = ValueSet::union_all(&literals);
                let f = ValueSet::scalars().difference(&t);
                Some(Truth { t, f })
            }
            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } if !cx.dated => {
                let lo = Self::number(left, cx)?;
                let hi = Self::number(right, cx)?;
                let lo = match left_bracket {
                    Bracket::LeftSquareBracket => Bound::Inclusive(lo),
                    Bracket::LeftParenthesis => Bound::Exclusive(lo),
                    _ => return None,
                };
                let hi = match right_bracket {
                    Bracket::RightSquareBracket => Bound::Inclusive(hi),
                    Bracket::RightParenthesis => Bound::Exclusive(hi),
                    _ => return None,
                };
                Some(Self::numeric(NumberSet::from_intervals(vec![
                    Interval::new(lo, hi),
                ])))
            }
            _ => None,
        }
    }

    fn literal(node: &Node, cx: &Scope) -> Option<ValueSet> {
        match Self::unwrap(node) {
            Node::Null => Some(ValueSet::null()),
            Node::Bool(b) => Some(ValueSet::bool(*b)),
            Node::String(s) if !cx.dated => Some(ValueSet::string(s)),
            other => Self::number(other, cx).map(ValueSet::number),
        }
    }

    fn number(node: &Node, cx: &Scope) -> Option<Decimal> {
        if cx.dated {
            return match Self::unwrap(node) {
                Node::String(s) => DateDay::seconds(s),
                _ => None,
            };
        }
        match Self::unwrap(node) {
            Node::Number(n) => Some(*n),
            Node::Unary {
                operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                node,
            } => match Self::unwrap(node) {
                Node::Number(n) => Some(-*n),
                _ => None,
            },
            Node::Unary {
                operator: Operator::Arithmetic(ArithmeticOperator::Add),
                node,
            } => match Self::unwrap(node) {
                Node::Number(n) => Some(*n),
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn parse(source: &str) -> CellConstraint {
        CellConstraint::parse(&mut IntelliSense::new(), source, true, true, false)
    }

    fn known(source: &str) -> ValueSet {
        match parse(source) {
            CellConstraint::Known(set) => set,
            other => panic!("{source}: expected known, got {other:?}"),
        }
    }

    fn n(s: &str) -> ValueSet {
        ValueSet::number(Decimal::from_str(s).expect("decimal"))
    }

    #[test]
    fn literals_and_lists() {
        assert_eq!(known("\"gold\""), ValueSet::string("gold"));
        assert_eq!(known("5"), n("5"));
        assert_eq!(known("-5"), n("-5"));
        assert_eq!(known("true"), ValueSet::bool(true));
        assert_eq!(known("null"), ValueSet::null());
        let list = known("\"a\", \"b\"");
        assert!(ValueSet::string("a").is_subset(&list));
        assert!(ValueSet::string("b").is_subset(&list));
        assert!(!ValueSet::string("c").intersects(&list));
        assert_eq!(known("[\"a\", \"b\"]"), list);
    }

    #[test]
    fn comparisons_accept_only_numbers() {
        let gt = known("> 5");
        assert!(n("6").is_subset(&gt));
        assert!(!n("5").intersects(&gt));
        assert!(!ValueSet::null().intersects(&gt));
        assert!(!ValueSet::string("x").intersects(&gt));
        assert_eq!(known("$ > 5"), gt);
        assert_eq!(known("5 < $"), gt);
    }

    #[test]
    fn negations_follow_runtime_errors() {
        let ne = known("!= 5");
        assert!(ValueSet::null().is_subset(&ne));
        assert!(ValueSet::string("x").is_subset(&ne));
        assert!(!n("5").intersects(&ne));
        let mut other = ValueSet::empty();
        other.other = true;
        assert!(other.is_subset(&ne));

        let not_in = known("not in [\"a\", \"b\"]");
        assert!(ValueSet::null().is_subset(&not_in));
        assert!(n("1").is_subset(&not_in));
        assert!(!ValueSet::string("a").intersects(&not_in));
        assert!(!other.intersects(&not_in));

        let not_gt = known("not ($ > 5)");
        assert!(n("5").is_subset(&not_gt));
        assert!(!ValueSet::null().intersects(&not_gt));
    }

    #[test]
    fn intervals_and_conjunctions() {
        let closed = known("[18..65)");
        assert!(n("18").is_subset(&closed));
        assert!(!n("65").intersects(&closed));
        assert!(known("> 5 and < 3").is_empty());
        assert!(known("[5..3]").is_empty());
        let either = known("< 0, > 10");
        assert!(n("-1").is_subset(&either));
        assert!(n("11").is_subset(&either));
        assert!(!n("5").intersects(&either));
    }

    #[test]
    fn unknown_cells_are_opaque_atoms() {
        let a = parse("some($, # > 3)");
        let b = parse("some($,   # > 3)");
        let c = parse("len($) > 3");
        assert!(matches!(a, CellConstraint::Opaque(_)));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(matches!(parse("customer.tier"), CellConstraint::Opaque(_)));
        assert!(matches!(
            parse("> \"2024-01-01\""),
            CellConstraint::Opaque(_)
        ));
        assert!(matches!(parse("> 5 and"), CellConstraint::Opaque(_)));
        assert_eq!(parse(""), CellConstraint::Any);
        assert_ne!(parse("rand(10) > 5"), parse("rand(10) > 5"));
    }

    #[test]
    fn non_analyzable_columns_are_opaque() {
        let a = CellConstraint::parse(&mut IntelliSense::new(), "5", true, false, false);
        assert!(matches!(a, CellConstraint::Opaque(_)));
    }
}
