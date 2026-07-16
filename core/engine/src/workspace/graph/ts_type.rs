use std::cell::RefCell;
use std::rc::Rc;

use ahash::{HashMap, HashMapExt};
use swc_common::input::StringInput;
use swc_common::source_map::SmallPos;
use swc_common::BytePos;
use swc_ecma_ast::{
    Decl, ModuleDecl, ModuleItem, Stmt, TsFnOrConstructorType, TsIntersectionType, TsKeywordType,
    TsKeywordTypeKind, TsLit, TsLitType, TsType, TsTypeElement, TsTypeLit, TsTypeRef,
    TsUnionOrIntersectionType, TsUnionType,
};
use swc_ecma_parser::{lexer::Lexer, Parser, Syntax, TsSyntax};
use zen_expression::variable::VariableType;

pub(crate) struct TsTypeParser;

impl TsTypeParser {
    pub(crate) fn variable_type(source: &str) -> Option<VariableType> {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return None;
        }
        let wrapped = format!("type __T = {trimmed};");
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax::default()),
            Default::default(),
            StringInput::new(
                &wrapped,
                BytePos::from_usize(0),
                BytePos::from_usize(wrapped.len()),
            ),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_typescript_module().ok()?;
        if !parser.take_errors().is_empty() {
            return None;
        }
        let alias = module.body.iter().find_map(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::TsTypeAlias(alias)))
                if alias.id.sym.as_ref() == "__T" =>
            {
                Some(alias)
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::TsTypeAlias(alias) if alias.id.sym.as_ref() == "__T" => Some(alias),
                _ => None,
            },
            _ => None,
        })?;
        Some(Self::convert(&alias.type_ann))
    }

    fn convert(ty: &TsType) -> VariableType {
        match ty {
            TsType::TsKeywordType(keyword) => Self::keyword(keyword),
            TsType::TsLitType(literal) => Self::literal(literal),
            TsType::TsArrayType(array) => Self::convert(&array.elem_type).array(),
            TsType::TsTupleType(tuple) => tuple
                .elem_types
                .iter()
                .map(|e| Self::convert(&e.ty))
                .reduce(|acc, t| acc.merge(&t))
                .unwrap_or(VariableType::Any)
                .array(),
            TsType::TsTypeLit(literal) => Self::type_literal(literal),
            TsType::TsUnionOrIntersectionType(kind) => match kind {
                TsUnionOrIntersectionType::TsUnionType(union) => Self::union(union),
                TsUnionOrIntersectionType::TsIntersectionType(intersection) => {
                    Self::intersection(intersection)
                }
            },
            TsType::TsParenthesizedType(inner) => Self::convert(&inner.type_ann),
            TsType::TsOptionalType(inner) => {
                VariableType::Nullable(Rc::new(Self::convert(&inner.type_ann)))
            }
            TsType::TsRestType(inner) => Self::convert(&inner.type_ann),
            TsType::TsTypeOperator(operator) => Self::convert(&operator.type_ann),
            TsType::TsTypeRef(reference) => Self::reference(reference),
            TsType::TsFnOrConstructorType(TsFnOrConstructorType::TsFnType(_)) => VariableType::Any,
            _ => VariableType::Any,
        }
    }

    fn keyword(keyword: &TsKeywordType) -> VariableType {
        match keyword.kind {
            TsKeywordTypeKind::TsNumberKeyword | TsKeywordTypeKind::TsBigIntKeyword => {
                VariableType::Number
            }
            TsKeywordTypeKind::TsStringKeyword => VariableType::String,
            TsKeywordTypeKind::TsBooleanKeyword => VariableType::Bool,
            TsKeywordTypeKind::TsNullKeyword
            | TsKeywordTypeKind::TsUndefinedKeyword
            | TsKeywordTypeKind::TsVoidKeyword
            | TsKeywordTypeKind::TsNeverKeyword => VariableType::Null,
            _ => VariableType::Any,
        }
    }

    fn literal(literal: &TsLitType) -> VariableType {
        match &literal.lit {
            TsLit::Str(value) => {
                VariableType::Const(Rc::from(value.value.as_str().unwrap_or_default()))
            }
            TsLit::Number(_) => VariableType::Number,
            TsLit::Bool(_) => VariableType::Bool,
            TsLit::BigInt(_) => VariableType::Number,
            TsLit::Tpl(_) => VariableType::String,
        }
    }

    fn type_literal(literal: &TsTypeLit) -> VariableType {
        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
        for member in &literal.members {
            let TsTypeElement::TsPropertySignature(property) = member else {
                return VariableType::Any;
            };
            let name: Rc<str> = match property.key.as_ref() {
                swc_ecma_ast::Expr::Ident(ident) => Rc::from(ident.sym.as_ref()),
                swc_ecma_ast::Expr::Lit(swc_ecma_ast::Lit::Str(value)) => {
                    Rc::from(value.value.as_str().unwrap_or_default())
                }
                _ => return VariableType::Any,
            };
            let mut resolved = property
                .type_ann
                .as_ref()
                .map(|ann| Self::convert(&ann.type_ann))
                .unwrap_or(VariableType::Any);
            if property.optional {
                resolved = super::wrap_optional(resolved);
            }
            fields.insert(name, resolved);
        }
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }

    fn union(union: &TsUnionType) -> VariableType {
        union
            .types
            .iter()
            .map(|t| Self::convert(t))
            .reduce(|acc, t| acc.merge(&t))
            .unwrap_or(VariableType::Any)
    }

    fn intersection(intersection: &TsIntersectionType) -> VariableType {
        intersection
            .types
            .iter()
            .map(|t| Self::convert(t))
            .reduce(|acc, t| acc.merge(&t))
            .unwrap_or(VariableType::Any)
    }

    fn reference(reference: &TsTypeRef) -> VariableType {
        let Some(ident) = reference.type_name.as_ident() else {
            return VariableType::Any;
        };
        let first_param = || {
            reference
                .type_params
                .as_ref()
                .and_then(|params| params.params.first())
                .map(|param| Self::convert(param))
                .unwrap_or(VariableType::Any)
        };
        match ident.sym.as_ref() {
            "Array" | "ReadonlyArray" => first_param().array(),
            "Promise" | "Awaited" | "Readonly" => first_param(),
            "Date" => VariableType::Date,
            _ => VariableType::Any,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TsTypeParser;
    use zen_expression::variable::VariableType;

    fn parse(source: &str) -> VariableType {
        TsTypeParser::variable_type(source).expect("parses")
    }

    #[test]
    fn keywords_and_arrays() {
        assert!(matches!(parse("number"), VariableType::Number));
        assert!(matches!(parse("string[]"), VariableType::Array(_)));
        assert!(matches!(parse("Array<boolean>"), VariableType::Array(_)));
        assert!(matches!(parse("unknown"), VariableType::Any));
    }

    #[test]
    fn object_literals_with_optional() {
        let ty = parse("{ total: number; note?: string }");
        let VariableType::Object(fields) = &ty else {
            panic!("{ty:?}");
        };
        let fields = fields.borrow();
        assert!(matches!(fields.get("total"), Some(VariableType::Number)));
        assert!(matches!(
            fields.get("note"),
            Some(VariableType::Nullable(_))
        ));
    }

    #[test]
    fn string_literal_unions_become_enums() {
        let ty = parse("'gold' | 'basic'");
        assert!(
            matches!(ty, VariableType::Enum(_, ref v) if v.len() == 2),
            "{ty:?}"
        );
    }

    #[test]
    fn nullable_via_union() {
        let ty = parse("number | null");
        assert!(matches!(ty, VariableType::Nullable(_)), "{ty:?}");
    }

    #[test]
    fn promise_unwraps() {
        let ty = parse("Promise<{ total: number }>");
        assert!(matches!(ty, VariableType::Object(_)), "{ty:?}");
    }

    #[test]
    fn unsupported_becomes_any() {
        assert!(matches!(parse("Record<string, number>"), VariableType::Any));
        assert!(matches!(parse("Map<string, number>"), VariableType::Any));
    }

    #[test]
    fn tsc_realistic_output_parses() {
        let ty = parse(
            r#"{ doubled: { value: number; label: string; }[]; total: number; tiers: ("bulk" | "single")[]; }"#,
        );
        let VariableType::Object(fields) = &ty else {
            panic!("{ty:?}");
        };
        let fields = fields.borrow();
        assert!(
            matches!(fields.get("total"), Some(VariableType::Number)),
            "{fields:?}"
        );
        assert!(
            matches!(fields.get("doubled"), Some(VariableType::Array(_))),
            "{fields:?}"
        );
        assert!(
            matches!(fields.get("tiers"), Some(VariableType::Array(_))),
            "{fields:?}"
        );
    }

    #[test]
    fn garbage_is_none() {
        assert!(TsTypeParser::variable_type("{{{{").is_none());
        assert!(TsTypeParser::variable_type("").is_none());
    }
}
