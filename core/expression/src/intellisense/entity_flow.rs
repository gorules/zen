use std::rc::Rc;

use crate::functions::{ClosureFunction, FunctionKind};
use crate::lexer::{LogicalOperator, Operator};
use crate::parser::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSource {
    pub path: Vec<Rc<str>>,
    pub element: bool,
}

impl FlowSource {
    pub(crate) fn from_node(node: &Node) -> Option<FlowSource> {
        match node {
            Node::Parenthesized(inner) => Self::from_node(inner),
            Node::Identifier(_) | Node::Root => Self::path_of(node).map(|path| FlowSource {
                path,
                element: false,
            }),
            Node::Member { node: base, property } => match property {
                Node::String(_) => Self::path_of(node).map(|path| FlowSource {
                    path,
                    element: false,
                }),
                Node::Number(_) => {
                    let source = Self::from_node(base)?;
                    (!source.element).then_some(FlowSource {
                        path: source.path,
                        element: true,
                    })
                }
                _ => None,
            },
            Node::Slice { node: base, .. } => {
                let source = Self::from_node(base)?;
                (!source.element).then_some(source)
            }
            Node::FunctionCall {
                kind: FunctionKind::Closure(ClosureFunction::Filter),
                arguments,
            } => {
                let source = Self::from_node(arguments.first()?)?;
                (!source.element).then_some(source)
            }
            Node::Binary {
                left,
                operator: Operator::Logical(LogicalOperator::NullishCoalescing),
                right,
            } => Self::agreeing(left, right),
            Node::Conditional {
                on_true, on_false, ..
            } => Self::agreeing(on_true, on_false),
            _ => None,
        }
    }

    fn agreeing(a: &Node, b: &Node) -> Option<FlowSource> {
        let left = Self::from_node(a)?;
        let right = Self::from_node(b)?;
        (left == right).then_some(left)
    }

    fn path_of(node: &Node) -> Option<Vec<Rc<str>>> {
        match node {
            Node::Identifier(name) => Some(vec![Rc::from(*name)]),
            Node::Member { node: base, property } => {
                let mut path = Self::path_of(base)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intellisense::IntelliSense;

    fn flow(source: &str) -> Option<FlowSource> {
        IntelliSense::new().flow_source(source)
    }

    fn path(segments: &[&str]) -> Vec<Rc<str>> {
        segments.iter().map(|s| Rc::from(*s)).collect()
    }

    #[test]
    fn bare_path_is_identity() {
        assert_eq!(
            flow("customer.companies"),
            Some(FlowSource {
                path: path(&["customer", "companies"]),
                element: false,
            })
        );
    }

    #[test]
    fn filter_preserves_identity() {
        assert_eq!(
            flow("filter(customer.companies, $.revenue > 0)"),
            Some(FlowSource {
                path: path(&["customer", "companies"]),
                element: false,
            })
        );
    }

    #[test]
    fn index_yields_element() {
        assert_eq!(
            flow("customer.companies[0]"),
            Some(FlowSource {
                path: path(&["customer", "companies"]),
                element: true,
            })
        );
    }

    #[test]
    fn slice_preserves_array() {
        assert_eq!(
            flow("customer.companies[1:3]"),
            Some(FlowSource {
                path: path(&["customer", "companies"]),
                element: false,
            })
        );
    }

    #[test]
    fn filter_of_index_is_rejected() {
        assert_eq!(flow("filter(customer.companies[0], true)"), None);
    }

    #[test]
    fn index_of_index_is_rejected() {
        assert_eq!(flow("customer.companies[0][1]"), None);
    }

    #[test]
    fn nullish_with_agreeing_sides() {
        assert_eq!(
            flow("customer.companies ?? customer.companies"),
            Some(FlowSource {
                path: path(&["customer", "companies"]),
                element: false,
            })
        );
    }

    #[test]
    fn conditional_with_disagreeing_branches_is_rejected() {
        assert_eq!(
            flow("customer.age > 10 ? customer.companies : customer.orders"),
            None
        );
    }

    #[test]
    fn conditional_with_agreeing_branches() {
        assert_eq!(
            flow("customer.age > 10 ? customer.companies[0] : customer.companies[0]"),
            Some(FlowSource {
                path: path(&["customer", "companies"]),
                element: true,
            })
        );
    }

    #[test]
    fn map_erases_identity() {
        assert_eq!(
            flow("map(customer.companies as c, { name: c.name })"),
            None
        );
    }

    #[test]
    fn arithmetic_erases_identity() {
        assert_eq!(flow("customer.age * 2"), None);
    }

    #[test]
    fn filter_chain_through_member_path() {
        assert_eq!(
            flow("filter(customer.profitableCompanies, $.revenue > 100)[0]"),
            Some(FlowSource {
                path: path(&["customer", "profitableCompanies"]),
                element: true,
            })
        );
    }
}
