use std::rc::Rc;

use rust_decimal::Decimal;

use crate::lexer::{ArithmeticOperator, Bracket, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;

#[derive(Debug, Clone, PartialEq)]
pub enum ArmTest {
    Enum {
        path: Vec<Rc<str>>,
        values: Vec<Rc<str>>,
    },
    Bool {
        path: Vec<Rc<str>>,
        values: Vec<bool>,
    },
    Number {
        path: Vec<Rc<str>>,
        cover: NumberCover,
    },
    Default,
    Unrecognized,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NumberCover {
    segments: Vec<NumberSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct NumberSegment {
    lo: Bound,
    hi: Bound,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Bound {
    Unbounded,
    Inclusive(Decimal),
    Exclusive(Decimal),
}

enum Operand {
    Path(Vec<Rc<str>>),
    Num(Decimal),
    Str(Rc<str>),
    Bool(bool),
    Other,
}

impl ArmTest {
    pub(crate) fn from_node(node: &Node) -> ArmTest {
        match node {
            Node::Parenthesized(inner) => Self::from_node(inner),
            Node::Binary {
                left,
                operator,
                right,
            } => Self::binary(left, *operator, right),
            _ => ArmTest::Unrecognized,
        }
    }

    fn binary(left: &Node, operator: Operator, right: &Node) -> ArmTest {
        match operator {
            Operator::Comparison(ComparisonOperator::Equal) => Self::equality(left, right),
            Operator::Comparison(ComparisonOperator::In) => Self::in_set(left, right),
            Operator::Comparison(
                op @ (ComparisonOperator::LessThan
                | ComparisonOperator::LessThanOrEqual
                | ComparisonOperator::GreaterThan
                | ComparisonOperator::GreaterThanOrEqual),
            ) => Self::numeric(left, op, right),
            Operator::Logical(LogicalOperator::And) => {
                Self::and(Self::from_node(left), Self::from_node(right))
            }
            Operator::Logical(LogicalOperator::Or) => {
                Self::or(Self::from_node(left), Self::from_node(right))
            }
            _ => ArmTest::Unrecognized,
        }
    }

    fn equality(left: &Node, right: &Node) -> ArmTest {
        let (path, literal) = match (Operand::classify(left), Operand::classify(right)) {
            (Operand::Path(p), literal) => (p, literal),
            (literal, Operand::Path(p)) => (p, literal),
            _ => return ArmTest::Unrecognized,
        };
        match literal {
            Operand::Str(s) => ArmTest::Enum {
                path,
                values: vec![s],
            },
            Operand::Bool(b) => ArmTest::Bool {
                path,
                values: vec![b],
            },
            Operand::Num(n) => ArmTest::Number {
                path,
                cover: NumberCover::point(n),
            },
            _ => ArmTest::Unrecognized,
        }
    }

    fn in_set(left: &Node, right: &Node) -> ArmTest {
        let Operand::Path(path) = Operand::classify(left) else {
            return ArmTest::Unrecognized;
        };
        if let Node::Interval {
            left: lo,
            right: hi,
            left_bracket,
            right_bracket,
        } = Operand::unwrap(right)
        {
            let (Operand::Num(lo), Operand::Num(hi)) =
                (Operand::classify(lo), Operand::classify(hi))
            else {
                return ArmTest::Unrecognized;
            };
            return match NumberCover::interval(lo, *left_bracket, hi, *right_bracket) {
                Some(cover) => ArmTest::Number { path, cover },
                None => ArmTest::Unrecognized,
            };
        }
        let Node::Array(items) = Operand::unwrap(right) else {
            return ArmTest::Unrecognized;
        };
        if items.is_empty() {
            return ArmTest::Unrecognized;
        }
        if let Some(values) = items
            .iter()
            .map(|n| match Operand::unwrap(n) {
                Node::String(s) => Some(Rc::from(*s)),
                _ => None,
            })
            .collect::<Option<Vec<Rc<str>>>>()
        {
            return ArmTest::Enum { path, values };
        }
        if let Some(values) = items
            .iter()
            .map(|n| match Operand::unwrap(n) {
                Node::Bool(b) => Some(*b),
                _ => None,
            })
            .collect::<Option<Vec<bool>>>()
        {
            return ArmTest::Bool { path, values };
        }
        if let Some(values) = items
            .iter()
            .map(|n| match Operand::classify(n) {
                Operand::Num(d) => Some(d),
                _ => None,
            })
            .collect::<Option<Vec<Decimal>>>()
        {
            let mut numbers = values.into_iter();
            let Some(first) = numbers.next() else {
                return ArmTest::Unrecognized;
            };
            let cover = numbers.fold(NumberCover::point(first), |mut cover, n| {
                cover.merged_with(&NumberCover::point(n));
                cover
            });
            return ArmTest::Number { path, cover };
        }
        ArmTest::Unrecognized
    }

    fn numeric(left: &Node, op: ComparisonOperator, right: &Node) -> ArmTest {
        let (path, num, op) = match (Operand::classify(left), Operand::classify(right)) {
            (Operand::Path(p), Operand::Num(n)) => (p, n, op),
            (Operand::Num(n), Operand::Path(p)) => (p, n, Self::flip(op)),
            _ => return ArmTest::Unrecognized,
        };
        match NumberCover::single(op, num) {
            Some(cover) => ArmTest::Number { path, cover },
            None => ArmTest::Unrecognized,
        }
    }

    fn and(left: ArmTest, right: ArmTest) -> ArmTest {
        match (left, right) {
            (
                ArmTest::Number {
                    path: pa,
                    cover: ca,
                },
                ArmTest::Number {
                    path: pb,
                    cover: cb,
                },
            ) if pa == pb => match ca.intersect(&cb) {
                Some(cover) => ArmTest::Number { path: pa, cover },
                None => ArmTest::Unrecognized,
            },
            _ => ArmTest::Unrecognized,
        }
    }

    fn or(left: ArmTest, right: ArmTest) -> ArmTest {
        match (left, right) {
            (
                ArmTest::Enum {
                    path: pa,
                    values: mut va,
                },
                ArmTest::Enum {
                    path: pb,
                    values: vb,
                },
            ) if pa == pb => {
                va.extend(vb);
                ArmTest::Enum {
                    path: pa,
                    values: va,
                }
            }
            (
                ArmTest::Bool {
                    path: pa,
                    values: mut va,
                },
                ArmTest::Bool {
                    path: pb,
                    values: vb,
                },
            ) if pa == pb => {
                va.extend(vb);
                ArmTest::Bool {
                    path: pa,
                    values: va,
                }
            }
            (
                ArmTest::Number {
                    path: pa,
                    cover: mut ca,
                },
                ArmTest::Number {
                    path: pb,
                    cover: cb,
                },
            ) if pa == pb => {
                ca.merged_with(&cb);
                ArmTest::Number {
                    path: pa,
                    cover: ca,
                }
            }
            _ => ArmTest::Unrecognized,
        }
    }

    fn flip(op: ComparisonOperator) -> ComparisonOperator {
        match op {
            ComparisonOperator::LessThan => ComparisonOperator::GreaterThan,
            ComparisonOperator::LessThanOrEqual => ComparisonOperator::GreaterThanOrEqual,
            ComparisonOperator::GreaterThan => ComparisonOperator::LessThan,
            ComparisonOperator::GreaterThanOrEqual => ComparisonOperator::LessThanOrEqual,
            other => other,
        }
    }
}

impl Operand {
    fn classify(node: &Node) -> Operand {
        match Self::unwrap(node) {
            Node::Number(n) => Operand::Num(*n),
            Node::String(s) => Operand::Str(Rc::from(*s)),
            Node::Bool(b) => Operand::Bool(*b),
            Node::Unary {
                operator: Operator::Arithmetic(ArithmeticOperator::Subtract),
                node,
            } => match Self::unwrap(node) {
                Node::Number(n) => Operand::Num(-*n),
                _ => Operand::Other,
            },
            other => match Self::extract_path(other) {
                Some(path) => Operand::Path(path),
                None => Operand::Other,
            },
        }
    }

    fn unwrap<'a, 'n>(node: &'a Node<'n>) -> &'a Node<'n> {
        match node {
            Node::Parenthesized(inner) => Self::unwrap(inner),
            other => other,
        }
    }

    fn extract_path(node: &Node) -> Option<Vec<Rc<str>>> {
        match node {
            Node::Identifier(name) => Some(vec![Rc::from(*name)]),
            Node::Member { node, property } => {
                let mut path = Self::extract_path(node)?;
                match property {
                    Node::String(key) => {
                        path.push(Rc::from(*key));
                        Some(path)
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

impl NumberCover {
    fn point(n: Decimal) -> Self {
        Self {
            segments: vec![NumberSegment {
                lo: Bound::Inclusive(n),
                hi: Bound::Inclusive(n),
            }],
        }
    }

    fn interval(lo: Decimal, left: Bracket, hi: Decimal, right: Bracket) -> Option<Self> {
        let lo = match left {
            Bracket::LeftSquareBracket => Bound::Inclusive(lo),
            Bracket::LeftParenthesis => Bound::Exclusive(lo),
            _ => return None,
        };
        let hi = match right {
            Bracket::RightSquareBracket => Bound::Inclusive(hi),
            Bracket::RightParenthesis => Bound::Exclusive(hi),
            _ => return None,
        };
        Some(Self {
            segments: vec![NumberSegment { lo, hi }],
        })
    }

    fn single(op: ComparisonOperator, n: Decimal) -> Option<Self> {
        let segment = match op {
            ComparisonOperator::LessThan => NumberSegment {
                lo: Bound::Unbounded,
                hi: Bound::Exclusive(n),
            },
            ComparisonOperator::LessThanOrEqual => NumberSegment {
                lo: Bound::Unbounded,
                hi: Bound::Inclusive(n),
            },
            ComparisonOperator::GreaterThan => NumberSegment {
                lo: Bound::Exclusive(n),
                hi: Bound::Unbounded,
            },
            ComparisonOperator::GreaterThanOrEqual => NumberSegment {
                lo: Bound::Inclusive(n),
                hi: Bound::Unbounded,
            },
            _ => return None,
        };
        Some(Self {
            segments: vec![segment],
        })
    }

    pub fn points(&self) -> Option<Vec<Decimal>> {
        self.segments
            .iter()
            .map(|s| match (s.lo, s.hi) {
                (Bound::Inclusive(lo), Bound::Inclusive(hi)) if lo == hi => Some(lo),
                _ => None,
            })
            .collect()
    }

    pub fn merged_with(&mut self, other: &NumberCover) {
        self.segments.extend(other.segments.iter().copied());
    }

    fn intersect(&self, other: &NumberCover) -> Option<NumberCover> {
        let ([a], [b]) = (self.segments.as_slice(), other.segments.as_slice()) else {
            return None;
        };
        Some(NumberCover {
            segments: vec![NumberSegment {
                lo: Bound::tighter_lo(a.lo, b.lo),
                hi: Bound::tighter_hi(a.hi, b.hi),
            }],
        })
    }

    pub fn is_total(&self) -> bool {
        let mut segments = self.segments.clone();
        segments.sort_by(|a, b| Bound::lo_rank(a.lo).cmp(&Bound::lo_rank(b.lo)));
        let Some((first, rest)) = segments.split_first() else {
            return false;
        };
        if first.lo != Bound::Unbounded {
            return false;
        }
        let mut frontier = first.hi;
        for segment in rest {
            if frontier == Bound::Unbounded {
                return true;
            }
            if !Bound::connects(frontier, segment.lo) {
                return false;
            }
            frontier = Bound::wider_hi(frontier, segment.hi);
        }
        frontier == Bound::Unbounded
    }
}

impl Bound {
    fn lo_rank(self) -> (u8, Decimal, u8) {
        match self {
            Bound::Unbounded => (0, Decimal::ZERO, 0),
            Bound::Inclusive(n) => (1, n, 0),
            Bound::Exclusive(n) => (1, n, 1),
        }
    }

    fn hi_rank(self) -> (Decimal, u8) {
        match self {
            Bound::Exclusive(n) => (n, 0),
            Bound::Inclusive(n) => (n, 1),
            Bound::Unbounded => (Decimal::MAX, 2),
        }
    }

    fn tighter_lo(a: Bound, b: Bound) -> Bound {
        match (a, b) {
            (Bound::Unbounded, other) | (other, Bound::Unbounded) => other,
            _ if a.lo_rank() >= b.lo_rank() => a,
            _ => b,
        }
    }

    fn tighter_hi(a: Bound, b: Bound) -> Bound {
        match (a, b) {
            (Bound::Unbounded, other) | (other, Bound::Unbounded) => other,
            _ if a.hi_rank() <= b.hi_rank() => a,
            _ => b,
        }
    }

    fn wider_hi(a: Bound, b: Bound) -> Bound {
        match (a, b) {
            (Bound::Unbounded, _) | (_, Bound::Unbounded) => Bound::Unbounded,
            _ if a.hi_rank() >= b.hi_rank() => a,
            _ => b,
        }
    }

    fn connects(frontier_hi: Bound, next_lo: Bound) -> bool {
        let (Bound::Inclusive(f) | Bound::Exclusive(f), Bound::Inclusive(l) | Bound::Exclusive(l)) =
            (frontier_hi, next_lo)
        else {
            return true;
        };
        if l < f {
            return true;
        }
        if l > f {
            return false;
        }
        !(matches!(frontier_hi, Bound::Exclusive(_)) && matches!(next_lo, Bound::Exclusive(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intellisense::IntelliSense;

    fn test_of(source: &str) -> ArmTest {
        IntelliSense::new().arm_test(source)
    }

    fn path(segments: &[&str]) -> Vec<Rc<str>> {
        segments.iter().map(|s| Rc::from(*s)).collect()
    }

    fn number_cover(source: &str) -> NumberCover {
        match test_of(source) {
            ArmTest::Number { cover, .. } => cover,
            other => panic!("expected number cover, got {other:?}"),
        }
    }

    #[test]
    fn equality_string_is_enum() {
        assert_eq!(
            test_of("customer.segment == \"retail\""),
            ArmTest::Enum {
                path: path(&["customer", "segment"]),
                values: vec![Rc::from("retail")],
            }
        );
    }

    #[test]
    fn equality_literal_left_is_flipped_into_enum() {
        assert_eq!(
            test_of("\"retail\" == customer.segment"),
            ArmTest::Enum {
                path: path(&["customer", "segment"]),
                values: vec![Rc::from("retail")],
            }
        );
    }

    #[test]
    fn in_string_set_is_enum() {
        assert_eq!(
            test_of("customer.segment in [\"retail\", \"corporate\"]"),
            ArmTest::Enum {
                path: path(&["customer", "segment"]),
                values: vec![Rc::from("retail"), Rc::from("corporate")],
            }
        );
    }

    #[test]
    fn equality_bool_is_bool() {
        assert_eq!(
            test_of("customer.active == true"),
            ArmTest::Bool {
                path: path(&["customer", "active"]),
                values: vec![true],
            }
        );
    }

    #[test]
    fn empty_condition_is_default() {
        assert_eq!(test_of(""), ArmTest::Default);
    }

    #[test]
    fn not_equal_is_unrecognized() {
        assert_eq!(
            test_of("customer.segment != \"retail\""),
            ArmTest::Unrecognized
        );
    }

    #[test]
    fn not_in_is_unrecognized() {
        assert_eq!(test_of("customer.n not in [1, 2]"), ArmTest::Unrecognized);
    }

    #[test]
    fn garbage_is_unrecognized() {
        assert_eq!(test_of("customer."), ArmTest::Unrecognized);
    }

    #[test]
    fn same_property_and_is_interval_intersection() {
        let cover = number_cover("customer.p >= 10 and customer.p < 20");
        assert!(!cover.is_total());
    }

    #[test]
    fn different_property_and_is_guard() {
        assert_eq!(
            test_of("customer.p < 10 and customer.region == \"EU\""),
            ArmTest::Unrecognized
        );
    }

    #[test]
    fn same_property_or_is_enum_union() {
        assert_eq!(
            test_of("customer.segment == \"retail\" or customer.segment == \"corporate\""),
            ArmTest::Enum {
                path: path(&["customer", "segment"]),
                values: vec![Rc::from("retail"), Rc::from("corporate")],
            }
        );
    }

    #[test]
    fn number_tiling_no_gap_is_total() {
        let mut cover = number_cover("customer.age < 18");
        cover.merged_with(&number_cover("customer.age >= 18"));
        assert!(cover.is_total());
    }

    #[test]
    fn number_tiling_with_gap_is_not_total() {
        let mut cover = number_cover("customer.age < 18");
        cover.merged_with(&number_cover("customer.age > 18"));
        assert!(!cover.is_total());
    }

    #[test]
    fn number_tiling_inclusive_overlap_is_total() {
        let mut cover = number_cover("customer.age <= 18");
        cover.merged_with(&number_cover("customer.age >= 18"));
        assert!(cover.is_total());
    }

    #[test]
    fn number_point_fills_seam() {
        let mut cover = number_cover("customer.age < 18");
        cover.merged_with(&NumberCover::point(Decimal::from(18)));
        cover.merged_with(&number_cover("customer.age > 18"));
        assert!(cover.is_total());
    }

    #[test]
    fn lone_point_is_not_total() {
        assert!(!NumberCover::point(Decimal::from(5)).is_total());
    }

    #[test]
    fn disjoint_union_with_gap_is_not_total() {
        let mut cover = number_cover("customer.age < 10");
        cover.merged_with(&number_cover("customer.age >= 20"));
        assert!(!cover.is_total());
    }

    fn cell_of(source: &str) -> ArmTest {
        IntelliSense::new().cell_test(source)
    }

    fn cell_cover(source: &str) -> NumberCover {
        match cell_of(source) {
            ArmTest::Number { cover, .. } => cover,
            other => panic!("expected number cover, got {other:?}"),
        }
    }

    #[test]
    fn cell_string_literal_is_enum() {
        assert_eq!(
            cell_of("\"US\""),
            ArmTest::Enum {
                path: path(&["$"]),
                values: vec![Rc::from("US")],
            }
        );
    }

    #[test]
    fn cell_comma_list_is_enum_union() {
        assert_eq!(
            cell_of("\"US\", \"CA\""),
            ArmTest::Enum {
                path: path(&["$"]),
                values: vec![Rc::from("US"), Rc::from("CA")],
            }
        );
    }

    #[test]
    fn cell_in_array_is_enum() {
        assert_eq!(
            cell_of("in [\"US\", \"CA\"]"),
            ArmTest::Enum {
                path: path(&["$"]),
                values: vec![Rc::from("US"), Rc::from("CA")],
            }
        );
    }

    #[test]
    fn cell_bool_literal() {
        assert_eq!(
            cell_of("true"),
            ArmTest::Bool {
                path: path(&["$"]),
                values: vec![true],
            }
        );
    }

    #[test]
    fn cell_comparison_tiles() {
        let mut cover = cell_cover("< 18");
        cover.merged_with(&cell_cover(">= 18"));
        assert!(cover.is_total());
    }

    #[test]
    fn cell_closed_interval() {
        let mut cover = cell_cover("[0..18]");
        assert!(!cover.is_total());
        cover.merged_with(&cell_cover("> 18"));
        cover.merged_with(&cell_cover("< 0"));
        assert!(cover.is_total());
    }

    #[test]
    fn cell_open_interval_leaves_seam() {
        let mut cover = cell_cover("(0..18)");
        cover.merged_with(&cell_cover(">= 18"));
        cover.merged_with(&cell_cover("<= 0"));
        assert!(cover.is_total());
    }

    #[test]
    fn cell_negative_bounds() {
        let mut cover = cell_cover("[-10..10]");
        cover.merged_with(&cell_cover("> 10"));
        cover.merged_with(&cell_cover("< -10"));
        assert!(cover.is_total());
    }

    #[test]
    fn cell_and_intersects() {
        let cover = cell_cover(">= 10 and < 20");
        assert!(!cover.is_total());
    }

    #[test]
    fn cell_subpath_test_is_unrecognized() {
        assert_eq!(cell_of("$.foo == 1"), ArmTest::Unrecognized);
    }

    #[test]
    fn cell_function_condition_is_unrecognized() {
        assert_eq!(cell_of("startsWith($, \"a\")"), ArmTest::Unrecognized);
    }

    #[test]
    fn cell_empty_is_default() {
        assert_eq!(cell_of(""), ArmTest::Default);
    }
}
