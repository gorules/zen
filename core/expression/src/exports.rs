use std::collections::BTreeSet;
use std::rc::Rc;

use crate::expression::{Standard, Unary};
use crate::intellisense::dependency::{DependencyResolutionWalker, ReadDependency};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::variable::Variable;
use crate::{Expression, Isolate, IsolateError};

/// Evaluates a standard expression
pub fn evaluate_expression(expression: &str, context: Variable) -> Result<Variable, IsolateError> {
    Isolate::with_environment(context).run_standard(expression)
}

/// Evaluates a unary expression; Required: context must be an object with "$" key.
pub fn evaluate_unary_expression(
    expression: &str,
    context: Variable,
) -> Result<bool, IsolateError> {
    let Some(context_object_ref) = context.as_object() else {
        return Err(IsolateError::MissingContextReference);
    };

    let context_object = context_object_ref.borrow();
    if !context_object.contains_key(&Variable::dollar_key()) {
        return Err(IsolateError::MissingContextReference);
    }

    Isolate::with_environment(context).run_unary(expression)
}

/// Root-level context keys a standard expression can read, in stable order —
/// `None` when the expression addresses the whole context (`$root` and
/// friends) or cannot be analyzed, meaning no projection is safe. Callers use
/// this to avoid materialising data the expression provably never touches;
/// the set may over-include (harmless), never under-include.
pub fn expression_root_references(expression: &str) -> Option<Vec<String>> {
    let arena = bumpalo::Bump::new();
    let tokens = Lexer::new().tokenize(&arena, expression).ok()?;
    let parser = Parser::try_new(&tokens, &arena).ok()?;
    let result = parser.standard().with_metadata().parse();
    if !result.is_complete || result.root.has_error() {
        return None;
    }
    let metadata = result.metadata.unwrap_or_default();
    let dependencies = DependencyResolutionWalker::walk(result.root, &metadata);

    let mut roots = BTreeSet::new();
    if !collect_read_roots(&dependencies.reads, &mut roots) {
        return None;
    }
    for reference in &dependencies.references {
        if let Some(root) = reference.path.first() {
            if !insert_root(root, &mut roots) {
                return None;
            }
        }
    }
    Some(roots.into_iter().collect())
}

fn collect_read_roots(reads: &[ReadDependency], roots: &mut BTreeSet<String>) -> bool {
    for read in reads {
        match read {
            ReadDependency::Direct { path, .. } | ReadDependency::Unresolved { path, .. } => {
                let Some(root) = path.first() else {
                    return false;
                };
                if !insert_root(root, roots) {
                    return false;
                }
            }
            ReadDependency::Iteration {
                collection, reads, ..
            } => {
                let Some(root) = collection.first() else {
                    return false;
                };
                if !insert_root(root, roots) || !collect_read_roots(reads, roots) {
                    return false;
                }
            }
        }
    }
    true
}

fn insert_root(root: &Rc<str>, roots: &mut BTreeSet<String>) -> bool {
    if matches!(root.as_ref(), "$root" | "$" | "$nodes") {
        return false;
    }
    roots.insert(root.to_string());
    true
}

/// Compiles a standard expression
pub fn compile_expression(expression: &str) -> Result<Expression<Standard>, IsolateError> {
    Isolate::new().compile_standard(expression)
}

/// Compiles an unary expression
pub fn compile_unary_expression(expression: &str) -> Result<Expression<Unary>, IsolateError> {
    Isolate::new().compile_unary(expression)
}

#[cfg(test)]
mod test {
    use crate::evaluate_expression;
    use serde_json::json;

    #[test]
    fn example() {
        let context = json!({ "tax": { "percentage": 10 } });
        let tax_amount = evaluate_expression("50 * tax.percentage / 100", context.into()).unwrap();

        assert_eq!(tax_amount, json!(5).into());
    }
}
