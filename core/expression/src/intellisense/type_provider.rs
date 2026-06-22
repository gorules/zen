use crate::functions::internal::InternalFunction;
use crate::functions::registry::FunctionRegistry;
use crate::functions::{ClosureFunction, FunctionKind, MethodRegistry};
use crate::intellisense::scope::IntelliSenseScope;
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use crate::variable::VariableType;
use ahash::{HashMap, HashMapExt};
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::iter::once;
use std::ops::Deref;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeInfo {
    pub(crate) kind: VariableType,
    pub(crate) error: Option<String>,
}

impl Deref for TypeInfo {
    type Target = VariableType;

    fn deref(&self) -> &Self::Target {
        &self.kind
    }
}

impl Display for TypeInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl Default for TypeInfo {
    fn default() -> Self {
        Self {
            kind: VariableType::Any,
            error: None,
        }
    }
}

impl From<VariableType> for TypeInfo {
    fn from(value: VariableType) -> Self {
        Self {
            kind: value,
            error: None,
        }
    }
}

impl From<Rc<VariableType>> for TypeInfo {
    fn from(value: Rc<VariableType>) -> Self {
        Self {
            kind: value.deref().clone(),
            error: None,
        }
    }
}

#[derive(Debug)]
pub struct TypesProvider {
    types: HashMap<usize, TypeInfo>,
    strict: bool,
}

impl TypesProvider {
    pub fn generate(root: &Node, scope: IntelliSenseScope, strict: bool) -> Self {
        let mut s = Self {
            types: HashMap::new(),
            strict,
        };

        s.determine(root, scope);
        s
    }

    pub fn get_type(&self, node: &Node) -> Option<&TypeInfo> {
        let addr = node_address(node);
        self.types.get(&addr)
    }

    fn set_type(&mut self, node: &Node, type_info: TypeInfo) {
        let addr = node_address(node);
        self.types.insert(addr, type_info);
    }

    fn update_type<F>(&mut self, node: &Node, updater: F)
    where
        F: FnOnce(&mut TypeInfo),
    {
        let addr = node_address(node);
        if let Some(reference) = self.types.get_mut(&addr) {
            updater(reference)
        }
    }

    fn set_error(&mut self, node: &Node, message: String) {
        self.update_type(node, |typ| {
            typ.error = Some(message);
        });
    }

    fn maybe_nullable(kind: VariableType, nullable: bool) -> TypeInfo {
        if nullable
            && !matches!(
                kind,
                VariableType::Any | VariableType::Null | VariableType::Nullable(_)
            )
        {
            TypeInfo::from(VariableType::Nullable(Rc::new(kind)))
        } else {
            TypeInfo::from(kind)
        }
    }

    #[cfg_attr(not(target_family = "wasm"), recursive::recursive)]
    fn determine(&mut self, node: &Node, mut scope: IntelliSenseScope) -> TypeInfo {
        #[allow(non_snake_case)]
        let V = |vt: VariableType| TypeInfo::from(vt);
        #[allow(non_snake_case)]
        let Const = |v: &str| TypeInfo::from(VariableType::Const(Rc::from(v)));
        #[allow(non_snake_case)]
        let Error = |error: String| TypeInfo {
            kind: VariableType::Any,
            error: Some(error),
        };

        let mut on_fly_error: Option<String> = None;

        let mut node_type = match node {
            Node::Null => V(VariableType::Null),
            Node::Bool(_) => V(VariableType::Bool),
            Node::Number(_) => V(VariableType::Number),
            Node::String(s) => Const(*s),
            Node::TemplateString(parts) => {
                parts.iter().for_each(|n| {
                    self.determine(n, scope.clone());
                });

                V(VariableType::String)
            }

            Node::Pointer => V(scope.pointer_data.clone()),
            Node::Root => V(scope.root_data.clone()),

            Node::Slice { node, from, to } => {
                let node_type = self.determine(node, scope.clone());

                if let Some(f) = from {
                    let from_type = self.determine(f, scope.clone());
                    if !from_type.satisfies(&VariableType::Number) {
                        self.set_error(f, format!("Invalid slice index: expected a `number`, but found `{from_type}`."));
                    }
                }

                if let Some(t) = to {
                    let to_type = self.determine(t, scope.clone());
                    if !to_type.satisfies(&VariableType::Number) {
                        self.set_error(
                            t,
                            format!(
                                "Invalid slice index: expected a `number`, but found `{to_type}`."
                            ),
                        );
                    }
                }

                match node_type.kind.widen() {
                    VariableType::Any => V(VariableType::Any),
                    VariableType::Array(inner) => V(VariableType::Array(inner.clone())),
                    VariableType::String => V(VariableType::String),
                    _ => Error("Slice operation is only allowed on `string | any[]`".to_string()),
                }
            }

            Node::Array(items) => {
                let element_type = items
                    .iter()
                    .map(|n| self.determine(n, scope.clone()).kind)
                    .reduce(|acc, t| acc.merge(&t))
                    .unwrap_or(VariableType::Any);
                V(VariableType::Array(Rc::new(element_type)))
            }

            Node::Object(obj) => {
                let obj_type = obj
                    .iter()
                    .filter_map(|(k, v)| {
                        let key_type = self.determine(k, scope.clone());
                        Some((
                            key_type.kind.as_const_str()?,
                            self.determine(v, scope.clone()).kind,
                        ))
                    })
                    .collect();

                V(VariableType::Object(Rc::new(RefCell::new(obj_type))))
            }

            Node::Assignments { list, output } => {
                if matches!(scope.root_data, VariableType::Any | VariableType::Null) {
                    scope.root_data = VariableType::Object(Rc::new(RefCell::new(HashMap::new())));
                }

                let obj_type: HashMap<Rc<str>, VariableType> = list
                    .iter()
                    .filter_map(|(k, v)| {
                        let key_type = self.determine(k, scope.clone()).as_const_str()?;
                        let value_type = self.determine(v, scope.clone());

                        if let Some(new_var) = scope
                            .root_data
                            .dot_insert_detached(key_type.as_ref(), value_type.kind.shallow_clone())
                        {
                            scope.root_data = new_var;
                        };

                        Some((key_type, value_type.kind))
                    })
                    .collect();

                if let Some(output) = output {
                    self.determine(output, scope.clone())
                } else {
                    V(VariableType::Object(Rc::new(RefCell::new(obj_type))))
                }
            }

            Node::Identifier(i) => scope
                .get_alias(i)
                .map(|t| TypeInfo::from(t.clone()))
                .unwrap_or_else(|| TypeInfo::from(scope.root_data.get(i))),
            Node::Member { node, property } => {
                let node_type = self.determine(node, scope.clone());
                let property_type = self.determine(property, scope.clone());

                let node_nullable =
                    self.strict && matches!(node_type.kind, VariableType::Nullable(_));
                let resolved_kind = match &node_type.kind {
                    VariableType::Nullable(inner) => inner.as_ref(),
                    other => other,
                };

                match resolved_kind {
                    VariableType::Any => V(VariableType::Any),
                    VariableType::Null => V(VariableType::Null),
                    VariableType::Array(inner) => {
                        if !property_type.satisfies(&VariableType::Number) {
                            self.set_error(
                                property,
                                format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                            );
                        }

                        Self::maybe_nullable(inner.as_ref().clone(), self.strict)
                    }
                    VariableType::String | VariableType::Const(_) | VariableType::Enum(_, _) => {
                        if !property_type.satisfies(&VariableType::Number) {
                            self.set_error(
                                property,
                                format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                            );
                        }

                        Self::maybe_nullable(VariableType::String, self.strict)
                    }
                    VariableType::Object(obj) => {
                        if !property_type.satisfies(&VariableType::String) {
                            self.set_error(
                                property,
                                format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                            );
                        }

                        let obj = obj.borrow();
                        let field = match property_type.as_const_str() {
                            None => VariableType::Any,
                            Some(key) => match obj.get(&key) {
                                Some(t) => t.clone(),
                                None if !self.strict || obj.is_empty() => VariableType::Any,
                                None => {
                                    self.set_error(
                                        node,
                                        format!("'{key}' is not a valid member of `{node_type}`."),
                                    );
                                    VariableType::Any
                                }
                            },
                        };
                        Self::maybe_nullable(field, node_nullable)
                    }
                    _ => Error(format!("Expression of type `{property_type}` cannot be used to index `{node_type}`.")),
                }
            }
            Node::Binary {
                left,
                right,
                operator,
            } => {
                let left_type = self.determine(left, scope.clone());
                let right_type = self.determine(right, scope.clone());

                match operator {
                    Operator::Arithmetic(arith) => match arith {
                        ArithmeticOperator::Add => match (left_type.widen(), right_type.widen()) {
                            (VariableType::Number, VariableType::Number) => V(VariableType::Number),
                            (VariableType::String, VariableType::String) => V(VariableType::String),
                            (VariableType::Any, VariableType::Number | VariableType::String | VariableType::Any) => V(VariableType::Any),
                            (VariableType::Number | VariableType::String, VariableType::Any) => V(VariableType::Any),
                            _ => Error(format!(
                                "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                            )),
                        },
                        ArithmeticOperator::Subtract
                        | ArithmeticOperator::Multiply
                        | ArithmeticOperator::Divide
                        | ArithmeticOperator::Modulus
                        | ArithmeticOperator::Power => match (left_type.deref(), right_type.deref()) {
                            (VariableType::Number | VariableType::Any, VariableType::Number | VariableType::Any) => V(VariableType::Number),
                            _ => Error(format!(
                                "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                            )),
                        },
                    },
                    Operator::Logical(l) => match l {
                        LogicalOperator::And | LogicalOperator::Or | LogicalOperator::Not => {
                            match (left_type.deref(), right_type.deref()) {
                                (VariableType::Bool | VariableType::Any, VariableType::Bool | VariableType::Any) => V(VariableType::Bool),
                                _ => Error(format!(
                                    "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                                )),
                            }
                        }
                        LogicalOperator::NullishCoalescing => {
                            match &left_type.kind {
                                VariableType::Nullable(inner) => {
                                    V(inner.as_ref().merge(&right_type.kind))
                                }
                                VariableType::Null => {
                                    if self.strict {
                                        on_fly_error.replace(
                                            "Lint: Left-hand side of `??` is always null; the fallback is always used.".to_string(),
                                        );
                                    }
                                    TypeInfo::from(right_type.kind)
                                }
                                VariableType::Any => TypeInfo::from(left_type.kind),
                                _ => {
                                    if self.strict {
                                        on_fly_error.replace(format!(
                                            "Lint: Left-hand side of `??` is never null (`{left_type}`); the fallback is redundant."
                                        ));
                                    }
                                    TypeInfo::from(left_type.kind)
                                }
                            }
                        }
                    },
                    Operator::Comparison(comp) => match comp {
                        ComparisonOperator::Equal => {
                            match check_enum_comparison(&left_type, &right_type) {
                                Some(Some(err)) => { on_fly_error.replace(err); }
                                Some(None) => {}
                                None => {
                                    let always_false = Self::structured_comparison(&left_type, &right_type)
                                        || (types_disjoint(&left_type, &right_type) && !left_type.is_nullable() && !right_type.is_nullable() && !left_type.is_null() && !right_type.is_null());
                                    if always_false {
                                        on_fly_error.replace(format!(
                                            "Hint: Expression will always evaluate to `false` because `{left_type}` != `{right_type}`."
                                        ));
                                    }
                                }
                            }

                            V(VariableType::Bool)
                        },
                        ComparisonOperator::NotEqual => {
                            match check_enum_comparison(&left_type, &right_type) {
                                Some(Some(err)) => { on_fly_error.replace(err); }
                                Some(None) => {}
                                None => {
                                    let always_true = Self::structured_comparison(&left_type, &right_type)
                                        || (types_disjoint(&left_type, &right_type) && !left_type.is_nullable() && !right_type.is_nullable() && !left_type.is_null() && !right_type.is_null());
                                    if always_true {
                                        on_fly_error.replace(format!(
                                            "Hint: Expression will always evaluate to `true` because `{left_type}` != `{right_type}`."
                                        ));
                                    }
                                }
                            }

                            V(VariableType::Bool)
                        },
                        ComparisonOperator::LessThan
                        | ComparisonOperator::GreaterThan
                        | ComparisonOperator::LessThanOrEqual
                        | ComparisonOperator::GreaterThanOrEqual => match (left_type.deref(), right_type.deref()) {
                            (VariableType::Date | VariableType::Any, VariableType::Date | VariableType::Any) => V(VariableType::Bool),
                            (VariableType::Number | VariableType::Any, VariableType::Number | VariableType::Any) => V(VariableType::Bool),
                            _ => Error(format!(
                                "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                            )),
                        },
                        ComparisonOperator::In | ComparisonOperator::NotIn => match (left_type.widen(), right_type.widen()) {
                            (_, VariableType::Array(inner_type)) => {
                                match check_enum_comparison(&left_type, &inner_type) {
                                    Some(Some(err)) => { on_fly_error.replace(err); }
                                    Some(None) => {}
                                    None => {
                                        if types_disjoint(&left_type, &inner_type) {
                                            let expected = match comp {
                                                ComparisonOperator::In => "false",
                                                _ => "true"
                                            };

                                            on_fly_error.replace(format!(
                                                "Hint: Expression will always evaluate to `{expected}`. because array contains element of type `{inner_type}`, and `{left_type}` != `{inner_type}`."
                                            ));
                                        }
                                    }
                                }

                                V(VariableType::Bool)
                            },
                            (VariableType::Number | VariableType::Date, VariableType::Interval) => V(VariableType::Bool),
                            (VariableType::String, VariableType::Object(_)) => V(VariableType::Bool),
                            (VariableType::Any, _) => V(VariableType::Bool),
                            (_, VariableType::Any) => V(VariableType::Bool),
                            _ => Error(format!(
                                "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                            ))
                        }
                    },
                    _ => V(VariableType::Any),
                }
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                let condition_type = self.determine(condition, scope.clone());
                if !condition_type.satisfies(&VariableType::Bool) {
                    self.set_error(
                        condition,
                        format!("Ternary operator cannot be applied to type `{condition_type}`."),
                    );
                }

                let true_type = self.determine(on_true, scope.clone());
                let false_type = self.determine(on_false, scope.clone());

                V(true_type.kind.merge(&false_type.kind))
            }
            Node::Unary { node, operator } => {
                let node_type = self.determine(node, scope.clone());

                match operator {
                    Operator::Arithmetic(arith) => match arith {
                        ArithmeticOperator::Add | ArithmeticOperator::Subtract => {
                            if !node_type.satisfies(&VariableType::Number) {
                                self.set_error(node, format!("Operator `{operator}` cannot be applied to type `{node_type}`."))
                            }

                            V(VariableType::Number)
                        }
                        ArithmeticOperator::Multiply
                        | ArithmeticOperator::Divide
                        | ArithmeticOperator::Modulus
                        | ArithmeticOperator::Power => Error("Unsupported operator".to_string()),
                    },
                    Operator::Logical(logical) => match logical {
                        LogicalOperator::Not => {
                            if !node_type.satisfies(&VariableType::Bool) {
                                self.set_error(node, format!("Operator `{operator}` cannot be applied to type `{node_type}`."))
                            }

                            V(VariableType::Bool)
                        }
                        LogicalOperator::And
                        | LogicalOperator::Or
                        | LogicalOperator::NullishCoalescing => {
                            Error("Unsupported operator".to_string())
                        }
                    },
                    Operator::Comparison(_)
                    | Operator::Range
                    | Operator::Comma
                    | Operator::Slice
                    | Operator::Dot
                    | Operator::QuestionMark
                    | Operator::Assign
                    | Operator::Semi => Error("Unsupported operator".to_string()),
                }
            }
            Node::Interval { left, right, .. } => {
                let left_type = self.determine(left, scope.clone());
                if !left_type.satisfies(&VariableType::Number)
                    && !left_type.satisfies(&VariableType::Date)
                {
                    self.set_error(
                        left,
                        format!("Interval cannot be created from type `{left_type}`."),
                    )
                }

                let right_type = self.determine(right, scope.clone());
                if !right_type.satisfies(&VariableType::Number)
                    && !right_type.satisfies(&VariableType::Date)
                {
                    self.set_error(
                        right,
                        format!("Interval cannot be created from type `{right_type}`."),
                    )
                }

                V(VariableType::Interval)
            }
            Node::FunctionCall { arguments, kind } => {
                let is_closure_kind = matches!(kind, FunctionKind::Closure(_));
                let mut type_list: Vec<VariableType> = arguments
                    .iter()
                    .enumerate()
                    .map(|(i, n)| match is_closure_kind && i == 1 {
                        true => VariableType::Any,
                        false => self.determine(n, scope.clone()).kind,
                    })
                    .collect();

                if let FunctionKind::Closure(_) = kind {
                    let ptr_type = type_list[0]
                        .iterator()
                        .unwrap_or_else(|| Rc::new(VariableType::Any));
                    let ptr_type_inner = ptr_type.deref().clone();

                    let alias = match arguments[1] {
                        Node::Closure { alias, .. } => *alias,
                        _ => None,
                    };

                    let mut closure_scope = IntelliSenseScope {
                        pointer_data: ptr_type_inner.clone(),
                        current_data: scope.current_data.clone(),
                        root_data: scope.root_data.clone(),
                        aliases: scope.aliases.clone(),
                    };

                    if let Some(alias_name) = alias {
                        closure_scope
                            .aliases
                            .insert(Rc::from(alias_name), ptr_type_inner);
                    }

                    let new_type = self.determine(arguments[1], closure_scope);
                    type_list[1] = new_type.kind;
                }

                match kind {
                    FunctionKind::Internal(InternalFunction::Merge) => {
                        self.merge_typecheck(&type_list, arguments)
                    }
                    FunctionKind::Internal(InternalFunction::MergeDeep) => {
                        self.merge_deep_typecheck(&type_list, arguments)
                    }
                    FunctionKind::Internal(InternalFunction::Flatten) => {
                        self.flatten_typecheck(&type_list, arguments)
                    }
                    FunctionKind::Internal(InternalFunction::Values) => {
                        self.values_typecheck(&type_list, arguments)
                    }
                    FunctionKind::Internal(_) | FunctionKind::Deprecated(_) => {
                        let Some(def) = FunctionRegistry::get_definition(kind) else {
                            return V(VariableType::Any);
                        };

                        let typecheck = def.check_types(type_list.as_slice());
                        for (i, arg_error) in typecheck.arguments {
                            self.set_error(arguments[i], arg_error);
                        }

                        TypeInfo {
                            kind: typecheck.return_type,
                            error: typecheck.general,
                        }
                    }
                    FunctionKind::Closure(c) => {
                        if !type_list[0].is_iterable() {
                            self.set_error(
                                arguments[0],
                                format!("Argument of type `{}` is not `iterable`.", type_list[0]),
                            );
                        } else if self.strict && matches!(type_list[0], VariableType::Nullable(_)) {
                            self.set_error(
                                arguments[0],
                                format!(
                                    "Argument of type `{}` may be `null`; use `?? []` to provide a fallback.",
                                    type_list[0]
                                ),
                            );
                        }

                        if matches!(
                            c,
                            ClosureFunction::All
                                | ClosureFunction::None
                                | ClosureFunction::Some
                                | ClosureFunction::One
                                | ClosureFunction::Filter
                                | ClosureFunction::Count
                        ) {
                            if !type_list[1].satisfies(&VariableType::Bool) {
                                self.set_error(
                                    arguments[1],
                                    format!(
                                        "Callback must return a `bool`, but its return type is `{}`.",
                                        type_list[1]
                                    ),
                                );
                            }
                        }

                        match c {
                            ClosureFunction::All => V(VariableType::Bool),
                            ClosureFunction::Some => V(VariableType::Bool),
                            ClosureFunction::None => V(VariableType::Bool),
                            ClosureFunction::One => V(VariableType::Bool),
                            ClosureFunction::Filter => TypeInfo::from(type_list[0].clone()),
                            ClosureFunction::Count => V(VariableType::Number),
                            ClosureFunction::Map => {
                                V(VariableType::Array(Rc::new(type_list[1].clone())))
                            }
                            ClosureFunction::FlatMap => {
                                let body_ty = &type_list[1];
                                let element = match body_ty.iterator() {
                                    Some(inner) => inner.deref().clone(),
                                    None => body_ty.clone(),
                                };
                                V(VariableType::Array(Rc::new(element)))
                            }
                        }
                    }
                }
            }
            Node::MethodCall {
                this,
                arguments,
                kind,
            } => {
                let this_type = self.determine(this, scope.clone());
                let type_list: Vec<VariableType> = once(this_type.kind)
                    .chain(
                        arguments
                            .iter()
                            .map(|n| self.determine(n, scope.clone()).kind),
                    )
                    .collect();

                let Some(def) = MethodRegistry::get_definition(kind) else {
                    return V(VariableType::Any);
                };

                let typecheck = def.check_types(type_list.as_slice());
                for (i, arg_error) in typecheck.arguments {
                    if i == 0 {
                        self.set_error(this, arg_error);
                    } else {
                        self.set_error(arguments[i - 1], arg_error);
                    }
                }

                TypeInfo {
                    kind: typecheck.return_type,
                    error: typecheck.general,
                }
            }
            Node::Closure { body, .. } => TypeInfo::from(self.determine(body, scope.clone()).kind),
            Node::Parenthesized(c) => TypeInfo::from(self.determine(c, scope.clone()).kind),
            Node::Error { node, error } => match node {
                None => TypeInfo {
                    kind: VariableType::Any,
                    error: Some(error.to_string()),
                },
                Some(n) => {
                    let typ = self.determine(n, scope.clone());
                    TypeInfo {
                        kind: typ.kind,
                        error: Some(error.to_string()),
                    }
                }
            },
        };

        if let Some(error) = on_fly_error {
            node_type.error.replace(error);
        }

        self.set_type(node, node_type.clone());
        node_type
    }
}

fn types_disjoint(left: &VariableType, right: &VariableType) -> bool {
    let (left, _) = left.unwrap_nullable();
    let (right, _) = right.unwrap_nullable();
    match (value_set(left), value_set(right)) {
        (Some(l), Some(r)) => !l.iter().any(|v| r.contains(v)),
        _ => !left.satisfies(right),
    }
}

fn value_set(t: &VariableType) -> Option<Vec<Rc<str>>> {
    match t {
        VariableType::Const(c) => Some(vec![c.clone()]),
        VariableType::Enum(_, values) => Some(values.clone()),
        _ => None,
    }
}

fn check_enum_comparison(left: &VariableType, right: &VariableType) -> Option<Option<String>> {
    let (left, _) = left.unwrap_nullable();
    let (right, _) = right.unwrap_nullable();

    match (left, right) {
        (VariableType::Enum(_, values), VariableType::Const(c))
        | (VariableType::Const(c), VariableType::Enum(_, values)) => {
            if values.iter().any(|v| v == c) {
                Some(None)
            } else {
                let enum_type = match (left, right) {
                    (VariableType::Enum(_, _), _) => left,
                    _ => right,
                };
                Some(Some(format!(
                    "Value `\"{c}\"` is not a valid member of `{enum_type}`."
                )))
            }
        }
        (VariableType::Const(c1), VariableType::Const(c2)) => {
            if c1 == c2 {
                Some(None)
            } else {
                Some(Some(format!(
                    "Value `\"{c1}\"` will never equal `\"{c2}\"`."
                )))
            }
        }
        _ => None,
    }
}

#[allow(unused)]
fn node_address(node: &Node) -> usize {
    node as *const Node as usize
}

impl TypesProvider {
    fn merge_typecheck(&mut self, arg_types: &[VariableType], arg_nodes: &[&Node]) -> TypeInfo {
        if arg_types.len() != 1 {
            return TypeInfo {
                kind: VariableType::Any,
                error: Some(format!(
                    "Expected `1` arguments, got `{}`.",
                    arg_types.len()
                )),
            };
        }

        let Some(element) = arg_types[0].iterator() else {
            if let Some(node) = arg_nodes.first() {
                self.set_error(
                    node,
                    format!("`merge` expects an array, got `{}`", arg_types[0]),
                );
            }
            return TypeInfo::from(VariableType::Any);
        };

        Self::merge_kind_from_types(std::slice::from_ref(&*element))
    }

    fn merge_deep_typecheck(
        &mut self,
        arg_types: &[VariableType],
        arg_nodes: &[&Node],
    ) -> TypeInfo {
        if arg_types.len() != 1 {
            return TypeInfo {
                kind: VariableType::Any,
                error: Some(format!(
                    "Expected `1` arguments, got `{}`.",
                    arg_types.len()
                )),
            };
        }

        let Some(element) = arg_types[0].iterator() else {
            if let Some(node) = arg_nodes.first() {
                self.set_error(
                    node,
                    format!("`mergeDeep` expects an array, got `{}`", arg_types[0]),
                );
            }
            return TypeInfo::from(VariableType::Any);
        };

        Self::merge_deep_from_types(std::slice::from_ref(&*element))
    }

    fn flatten_typecheck(&mut self, arg_types: &[VariableType], arg_nodes: &[&Node]) -> TypeInfo {
        if arg_types.len() != 1 {
            return TypeInfo {
                kind: VariableType::Any,
                error: Some(format!(
                    "Expected `1` arguments, got `{}`.",
                    arg_types.len()
                )),
            };
        }
        let arg = &arg_types[0];
        let Some(outer_element) = arg.iterator() else {
            if let Some(node) = arg_nodes.first() {
                self.set_error(node, format!("`flatten` expects an array, got `{arg}`"));
            }
            return TypeInfo::from(VariableType::Any);
        };

        if self.strict && matches!(arg, VariableType::Nullable(_)) {
            if let Some(node) = arg_nodes.first() {
                self.set_error(
                    node,
                    format!("Argument of type `{arg}` may be `null`; use `?? []` to provide a fallback."),
                );
            }
        }

        let element = Self::flatten_contribution_for(&outer_element);
        TypeInfo::from(VariableType::Array(Rc::new(element)))
    }

    /// Typecheck `values(obj)` → `Array<union of field types>`.
    /// Falls back to `Array<Any>` if the object has no known fields.
    fn values_typecheck(&mut self, arg_types: &[VariableType], arg_nodes: &[&Node]) -> TypeInfo {
        if arg_types.len() != 1 {
            return TypeInfo {
                kind: VariableType::Any,
                error: Some(format!(
                    "Expected `1` arguments, got `{}`.",
                    arg_types.len()
                )),
            };
        }
        let arg = &arg_types[0];
        let (inner_ty, _) = arg.unwrap_nullable();
        let VariableType::Object(obj) = inner_ty else {
            if !matches!(inner_ty, VariableType::Any) {
                if let Some(node) = arg_nodes.first() {
                    self.set_error(node, format!("`values` expects an object, got `{arg}`"));
                }
            }
            return TypeInfo::from(VariableType::Array(Rc::new(VariableType::Any)));
        };
        let fields = obj.borrow();
        let mut union: Option<VariableType> = None;
        for v in fields.values() {
            union = Some(match union {
                None => v.clone(),
                Some(prev) => prev.merge(v),
            });
        }
        TypeInfo::from(VariableType::Array(Rc::new(
            union.unwrap_or(VariableType::Any),
        )))
    }

    fn structured_comparison(left: &VariableType, right: &VariableType) -> bool {
        matches!(
            (left, right),
            (VariableType::Object(_), VariableType::Object(_))
                | (VariableType::Array(_), VariableType::Array(_))
        )
    }

    fn merge_kind_from_types(types: &[VariableType]) -> TypeInfo {
        let mut saw_object = false;
        let mut saw_array = false;
        for ty in types {
            match ty.unwrap_nullable().0 {
                VariableType::Object(_) => saw_object = true,
                VariableType::Array(_) => saw_array = true,
                VariableType::Any | VariableType::Null => {}
                _ => {
                    return TypeInfo {
                        kind: VariableType::Any,
                        error: Some(format!("`merge` expects objects or arrays, got `{ty}`")),
                    };
                }
            }
        }

        if !saw_object && !saw_array {
            return TypeInfo::from(VariableType::Any);
        }

        if saw_object && !saw_array {
            let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
            for ty in types {
                if let VariableType::Object(obj) = ty.unwrap_nullable().0 {
                    for (k, v) in obj.borrow().iter() {
                        // Last write wins — matches the runtime.
                        fields.insert(k.clone(), v.clone());
                    }
                }
            }
            return TypeInfo::from(VariableType::Object(Rc::new(RefCell::new(fields))));
        }

        if saw_array && !saw_object {
            let mut element: Option<VariableType> = None;
            for ty in types {
                if let Some(inner) = ty.iterator() {
                    element = Some(match element {
                        None => inner.deref().clone(),
                        Some(prev) => prev.merge(&inner),
                    });
                }
            }
            return TypeInfo::from(VariableType::Array(Rc::new(
                element.unwrap_or(VariableType::Any),
            )));
        }

        TypeInfo {
            kind: VariableType::Any,
            error: Some(
                "`merge` expects all arguments to be objects, or all to be arrays".to_string(),
            ),
        }
    }

    fn merge_deep_from_types(types: &[VariableType]) -> TypeInfo {
        for ty in types {
            if !matches!(
                ty.unwrap_nullable().0,
                VariableType::Object(_) | VariableType::Any | VariableType::Null
            ) {
                return TypeInfo {
                    kind: VariableType::Any,
                    error: Some(format!("`mergeDeep` expects objects, got `{ty}`")),
                };
            }
        }
        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
        for ty in types {
            if let VariableType::Object(obj) = ty.unwrap_nullable().0 {
                for (k, v) in obj.borrow().iter() {
                    let new_value = match fields.remove(k) {
                        Some(existing) => Self::deep_merge_types(&existing, v),
                        None => v.clone(),
                    };
                    fields.insert(k.clone(), new_value);
                }
            }
        }
        TypeInfo::from(VariableType::Object(Rc::new(RefCell::new(fields))))
    }

    fn deep_merge_types(a: &VariableType, b: &VariableType) -> VariableType {
        match (a, b) {
            (VariableType::Object(oa), VariableType::Object(ob)) => {
                let mut fields: HashMap<Rc<str>, VariableType> = HashMap::new();
                for (k, v) in oa.borrow().iter() {
                    fields.insert(k.clone(), v.clone());
                }
                for (k, v) in ob.borrow().iter() {
                    let merged = match fields.remove(k) {
                        Some(existing) => Self::deep_merge_types(&existing, v),
                        None => v.clone(),
                    };
                    fields.insert(k.clone(), merged);
                }
                VariableType::Object(Rc::new(RefCell::new(fields)))
            }
            _ => b.clone(),
        }
    }

    fn flatten_contribution_for(t: &VariableType) -> VariableType {
        match t {
            VariableType::Nullable(inner) => {
                let inner_contribution = Self::flatten_contribution_for(inner);
                VariableType::Null.merge(&inner_contribution)
            }
            VariableType::Array(inner) => inner.deref().clone(),
            VariableType::Interval => VariableType::Number,
            scalar => scalar.clone(),
        }
    }
}
