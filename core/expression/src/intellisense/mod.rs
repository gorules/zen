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

#[derive(Serialize)]
pub struct IntelliSenseToken {
    pub span: (u32, u32),
    pub kind: Rc<VariableType>,
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

            match (metadata.get(&addr), type_data.get_type(node)) {
                (Some(md), Some(td)) => {
                    let mut r = results.borrow_mut();
                    r.push(IntelliSenseToken {
                        span: md.span,
                        error: td.error.clone(),
                        kind: td.kind.clone(),
                    })
                }
                _ => {}
            }
        });
        
        self.arena.with_mut(|a| a.reset());
        Some(results.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use crate::intellisense::IntelliSense;
    use serde_json::json;

    #[test]
    fn sample_test() {
        let mut is = IntelliSense::new();

        let data = json!({ "customer": { "firstName": "John", "lastName": "Doe" } });

        let typ = is.type_check("hello_world", data.into());
    }
}
