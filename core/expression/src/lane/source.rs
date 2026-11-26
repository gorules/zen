use crate::functions::{FunctionKind, InternalFunction};
use crate::lexer::{Identifier, Lexer, LogicalOperator, Operator, TokenKind};
use crate::parser::{Node, Parser};
use bumpalo::Bump;
use std::cell::Cell;

pub struct SourceInfo;

impl SourceInfo {
    fn any(source: &str, test: fn(&Node) -> bool) -> bool {
        Self::scan(source, false, test)
    }

    fn scan(source: &str, unary: bool, test: fn(&Node) -> bool) -> bool {
        if source.trim().is_empty() {
            return false;
        }
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let Ok(tokens) = lexer.tokenize(&bump, source) else {
            return true;
        };
        let Ok(parser) = Parser::try_new(&tokens, &bump) else {
            return true;
        };
        let parsed = match unary {
            true => parser.unary().parse(),
            false => parser.standard().parse(),
        };
        if parsed.error().is_err() {
            return true;
        }
        let found = Cell::new(false);
        let found_ref = &found;
        parsed.root.walk(move |node| {
            if test(node) {
                found_ref.set(true);
            }
        });
        found.get()
    }

    pub fn reads_whole_root(source: &str, unary: bool) -> bool {
        if source.trim().is_empty() {
            return false;
        }
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let Ok(tokens) = lexer.tokenize(&bump, source) else {
            return true;
        };
        let Ok(parser) = Parser::try_new(&tokens, &bump) else {
            return true;
        };
        let parsed = match unary {
            true => parser.unary().parse(),
            false => parser.standard().parse(),
        };
        if parsed.error().is_err() {
            return true;
        }
        let roots = Cell::new(0usize);
        let fields = Cell::new(0usize);
        let (roots_ref, fields_ref) = (&roots, &fields);
        parsed.root.walk(move |node| match node {
            Node::Root => roots_ref.set(roots_ref.get() + 1),
            Node::Member {
                node: Node::Root,
                property: Node::String(key),
            } if *key != "$" => fields_ref.set(fields_ref.get() + 1),
            _ => {}
        });
        roots.get() != fields.get()
    }

    pub fn with_dollar(source: &str, value: &str) -> Option<String> {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let tokens = lexer.tokenize(&bump, source).ok()?;
        let mut out = String::with_capacity(source.len() + value.len() + 2);
        let mut at = 0usize;
        for token in tokens.iter() {
            if token.kind != TokenKind::Identifier(Identifier::ContextReference) {
                continue;
            }
            let (start, end) = (token.span.0 as usize, token.span.1 as usize);
            out.push_str(source.get(at..start)?);
            out.push('(');
            out.push_str(value);
            out.push(')');
            at = end;
        }
        out.push_str(source.get(at..)?);
        Some(out)
    }

    pub fn scalar(source: &str) -> bool {
        if source.trim().is_empty() {
            return true;
        }
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let Ok(tokens) = lexer.tokenize(&bump, source) else {
            return false;
        };
        let Ok(parser) = Parser::try_new(&tokens, &bump) else {
            return false;
        };
        let parsed = parser.standard().parse();
        if parsed.error().is_err() {
            return false;
        }
        Self::scalar_node(parsed.root)
    }

    fn scalar_node(node: &Node) -> bool {
        match node {
            Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::TemplateString(_)
            | Node::Interval { .. }
            | Node::Unary { .. } => true,
            Node::Parenthesized(inner) => Self::scalar_node(inner),
            Node::Conditional {
                on_true, on_false, ..
            } => Self::scalar_node(on_true) && Self::scalar_node(on_false),
            Node::Binary {
                left,
                operator,
                right,
            } => match operator {
                Operator::Arithmetic(_) | Operator::Comparison(_) => true,
                Operator::Logical(
                    LogicalOperator::And | LogicalOperator::Or | LogicalOperator::NullishCoalescing,
                ) => Self::scalar_node(left) && Self::scalar_node(right),
                _ => false,
            },
            _ => false,
        }
    }

    pub fn draws(source: &str) -> bool {
        let test = |node: &Node| {
            matches!(
                node,
                Node::FunctionCall {
                    kind: FunctionKind::Internal(InternalFunction::Rand),
                    ..
                }
            )
        };
        source.contains("rand")
            && (Self::scan(source, false, test) || Self::scan(source, true, test))
    }

    pub fn parses(source: &str, unary: bool) -> bool {
        let bump = Bump::new();
        let mut lexer = Lexer::new();
        let Ok(tokens) = lexer.tokenize(&bump, source) else {
            return false;
        };
        let Ok(parser) = Parser::try_new(&tokens, &bump) else {
            return false;
        };
        let parsed = match unary {
            true => parser.unary().parse(),
            false => parser.standard().parse(),
        };
        parsed.error().is_ok()
    }

    pub fn reads_dollar(source: &str) -> bool {
        Self::any(source, |node| matches!(node, Node::Identifier("$")))
    }

    pub fn reads_env(source: &str) -> bool {
        Self::scan(source, true, |node| match node {
            Node::Identifier(name) => *name != "$",
            Node::Root => true,
            _ => false,
        })
    }

    pub fn reads_nodes_unary(source: &str) -> bool {
        Self::scan(source, true, |node| {
            matches!(node, Node::Identifier("$nodes") | Node::Root)
        })
    }

    pub fn reads_root_unary(source: &str) -> bool {
        Self::scan(source, true, |node| matches!(node, Node::Root))
    }

    pub fn reads_root(source: &str) -> bool {
        Self::any(source, |node| matches!(node, Node::Root))
    }

    pub fn reads_nodes(source: &str) -> bool {
        Self::any(source, |node| {
            matches!(node, Node::Identifier("$nodes") | Node::Root)
        })
    }
}
