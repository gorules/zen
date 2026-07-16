use std::sync::Arc;

use zen_expression::variable::VariableType;

use super::context::AnalysisContext;
use crate::workspace::types::{CursorTarget, DiagnosticCode};

pub(super) struct TypeCheck;

impl TypeCheck {
    pub(super) fn check_no_any(
        cx: &mut AnalysisContext,
        ty: &VariableType,
        expression_id: Option<Arc<str>>,
        target: Option<CursorTarget>,
        label: &str,
    ) {
        if Self::type_contains_any(ty) {
            let span = target.as_ref().map(|_| (0, label.chars().count() as u32));
            cx.error_with_target(
                DiagnosticCode::TypeMismatch,
                expression_id,
                span,
                target,
                format!("'{label}' resolves to `{ty}` which is `any` — give it a concrete type"),
            );
        }
    }

    fn type_contains_any(ty: &VariableType) -> bool {
        let mut visited: ahash::HashSet<*const ()> = ahash::HashSet::default();
        Self::contains_any_rec(ty, &mut visited)
    }

    fn contains_any_rec(ty: &VariableType, visited: &mut ahash::HashSet<*const ()>) -> bool {
        match ty {
            VariableType::Any => true,
            VariableType::Array(inner) | VariableType::Nullable(inner) => {
                Self::contains_any_rec(inner, visited)
            }
            VariableType::Object(obj) => {
                let ptr = std::rc::Rc::as_ptr(obj) as *const ();
                if !visited.insert(ptr) {
                    return false;
                }
                let fields = obj.borrow();
                let result = fields.values().any(|v| Self::contains_any_rec(v, visited));
                visited.remove(&ptr);
                result
            }
            _ => false,
        }
    }
}
