use crate::lexer::Lexer;
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
