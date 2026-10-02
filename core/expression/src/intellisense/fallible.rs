use crate::intellisense::type_provider::TypesProvider;
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use crate::variable::VariableType;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scalar {
    Number,
    String,
    Bool,
}

pub(crate) struct Fallible<'t> {
    types: &'t TypesProvider,
}

impl<'t> Fallible<'t> {
    pub(crate) fn new(types: &'t TypesProvider) -> Self {
        Self { types }
    }

    pub(crate) fn safe(&self, node: &Node) -> bool {
        match node {
            Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::Identifier(_)
            | Node::Root => true,
            Node::Parenthesized(inner) => self.safe(inner),
            Node::Member { node, property } => {
                matches!(property, Node::String(_) | Node::Number(_)) && self.safe(node)
            }
            Node::Array(items) => items.iter().all(|item| self.safe(item)),
            Node::Object(entries) => entries
                .iter()
                .all(|(key, value)| matches!(key, Node::String(_)) && self.safe(value)),
            Node::TemplateString(parts) => parts
                .iter()
                .all(|part| self.safe(part) && self.scalar(part).is_some()),
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => self.typed(condition, Scalar::Bool) && self.safe(on_true) && self.safe(on_false),
            Node::Unary { node, operator } => match operator {
                Operator::Logical(LogicalOperator::Not) => self.typed(node, Scalar::Bool),
                Operator::Arithmetic(ArithmeticOperator::Subtract | ArithmeticOperator::Add) => {
                    self.typed(node, Scalar::Number)
                }
                _ => false,
            },
            Node::Binary {
                left,
                operator,
                right,
            } => match operator {
                Operator::Logical(LogicalOperator::NullishCoalescing)
                | Operator::Comparison(ComparisonOperator::Equal | ComparisonOperator::NotEqual) => {
                    self.safe(left) && self.safe(right)
                }
                Operator::Logical(LogicalOperator::And | LogicalOperator::Or) => {
                    self.typed(left, Scalar::Bool) && self.typed(right, Scalar::Bool)
                }
                Operator::Arithmetic(
                    ArithmeticOperator::Subtract | ArithmeticOperator::Multiply,
                )
                | Operator::Comparison(
                    ComparisonOperator::LessThan
                    | ComparisonOperator::LessThanOrEqual
                    | ComparisonOperator::GreaterThan
                    | ComparisonOperator::GreaterThanOrEqual,
                ) => self.typed(left, Scalar::Number) && self.typed(right, Scalar::Number),
                Operator::Arithmetic(ArithmeticOperator::Divide | ArithmeticOperator::Modulus) => {
                    self.typed(left, Scalar::Number)
                        && matches!(right, Node::Number(divisor) if !divisor.is_zero())
                }
                Operator::Arithmetic(ArithmeticOperator::Add) => {
                    self.safe(left)
                        && self.safe(right)
                        && matches!(
                            (self.scalar(left), self.scalar(right)),
                            (Some(Scalar::Number), Some(Scalar::Number))
                                | (Some(Scalar::String), Some(Scalar::String))
                        )
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn typed(&self, node: &Node, scalar: Scalar) -> bool {
        self.safe(node) && self.scalar(node) == Some(scalar)
    }

    fn scalar(&self, node: &Node) -> Option<Scalar> {
        match &self.types.get_type(node)?.kind {
            VariableType::Number => Some(Scalar::Number),
            VariableType::String | VariableType::Const(_) | VariableType::Enum(..) => {
                Some(Scalar::String)
            }
            VariableType::Bool => Some(Scalar::Bool),
            _ => None,
        }
    }
}
