use crate::arena::UnsafeArena;
use crate::intellisense::scope::IntelliSenseScope;
use crate::intellisense::types::provider::TypesProvider;
use crate::lexer::Lexer;
use crate::parser::{Node, Parser};
use crate::variable::VariableType;
use serde::Serialize;
use std::cell::RefCell;
use std::rc::Rc;

mod scope;
mod types;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntelliSenseToken {
    pub span: (u32, u32),
    pub kind: Rc<VariableType>,
    pub node_kind: &'static str,
    pub error: Option<String>,
}

pub struct IntelliSense<'arena> {
    arena: UnsafeArena<'arena>,
    lexer: Lexer<'arena>,
}

impl<'arena> IntelliSense<'arena> {
    pub fn new() -> Self {
        Self {
            arena: UnsafeArena::new(),
            lexer: Lexer::new(),
        }
    }

    pub fn type_check(
        &mut self,
        source: &'arena str,
        data: &VariableType,
    ) -> Option<Vec<IntelliSenseToken>> {
        let arena = self.arena.get();

        let tokens = self.lexer.tokenize(source).ok()?;
        let parser = Parser::try_new(tokens, &arena).map(|p| p.standard()).ok()?;

        let parser_result = parser.with_metadata().parse();
        let ast = parser_result.root;
        let metadata = parser_result.metadata?;

        let type_data = TypesProvider::generate(
            ast,
            IntelliSenseScope {
                pointer_data: data,
                root_data: data,
                current_data: data,
            },
        );

        let results = RefCell::new(Vec::new());
        ast.walk(|node| {
            let addr = node as *const Node as usize;
            let mut r = results.borrow_mut();
            let typ = type_data.get_type(node);

            r.push(IntelliSenseToken {
                span: node
                    .span()
                    .or_else(|| metadata.get(&addr).map(|s| s.span))
                    .unwrap_or_default(),
                node_kind: node.into(),
                error: typ.map(|t| t.error.clone()).flatten(),
                kind: typ
                    .map(|t| t.kind.clone())
                    .unwrap_or_else(|| Rc::new(VariableType::Any)),
            });
        });

        self.arena.with_mut(|a| a.reset());
        Some(results.into_inner())
    }

    pub fn type_check_unary(
        &mut self,
        source: &'arena str,
        data: &VariableType,
    ) -> Option<Vec<IntelliSenseToken>> {
        let arena = self.arena.get();

        let tokens = self.lexer.tokenize(source).ok()?;
        let parser = Parser::try_new(tokens, &arena).map(|p| p.unary()).ok()?;

        let parser_result = parser.with_metadata().parse();
        let ast = parser_result.root;
        let metadata = parser_result.metadata?;

        let type_data = TypesProvider::generate(
            ast,
            IntelliSenseScope {
                pointer_data: data,
                root_data: data,
                current_data: data,
            },
        );

        let results = RefCell::new(Vec::new());
        ast.walk(|node| {
            let addr = node as *const Node as usize;
            let mut r = results.borrow_mut();
            let typ = type_data.get_type(node);

            r.push(IntelliSenseToken {
                span: metadata.get(&addr).map(|s| s.span).unwrap_or_default(),
                node_kind: node.into(),
                error: typ.map(|t| t.error.clone()).flatten(),
                kind: typ
                    .map(|t| t.kind.clone())
                    .unwrap_or_else(|| Rc::new(VariableType::Any)),
            });
        });

        self.arena.with_mut(|a| a.reset());
        Some(results.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use crate::intellisense::IntelliSense;
    use crate::variable::VariableType;
    use serde_json::json;

    #[test]
    fn sample_test() {
        let mut is = IntelliSense::new();

        let data = json!({ "customer": { "firstName": "John", "lastName": "Doe", "array": [{"a": 5}, {"a": 6}] } });
        let data_type: VariableType = data.into();

        let typ = is.type_check("customer.array[0]", &data_type);
        println!("{:?}", typ);
    }

    #[test]
    fn sample_test_unary() {
        let mut is = IntelliSense::new();

        let data = json!({ "customer": { "firstName": "John", "lastName": "Doe" }, "$": 10});
        let data_type: VariableType = data.into();

        let typ = is.type_check_unary("> 10", &data_type);
        println!("{typ:?}");
    }
}
