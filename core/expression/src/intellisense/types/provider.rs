use crate::functions::registry::FunctionRegistry;
use crate::functions::{ClosureFunction, FunctionKind, MethodRegistry};
use crate::intellisense::scope::IntelliSenseScope;
use crate::intellisense::types::type_info::TypeInfo;
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use crate::variable::VariableType;
use std::collections::HashMap;
use std::iter::once;
use std::ops::Deref;
use std::rc::Rc;

#[derive(Debug)]
pub struct TypesProvider {
    types: HashMap<usize, TypeInfo>,
}

impl TypesProvider {
    pub fn generate(root: &Node, scope: IntelliSenseScope) -> Self {
        let mut s = Self {
            types: HashMap::new(),
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

    #[cfg_attr(feature = "stack-protection", recursive::recursive)]
    fn determine(&mut self, node: &Node, scope: IntelliSenseScope) -> TypeInfo {
        #[allow(non_snake_case)]
        let V = |vt: VariableType| TypeInfo::from(vt);
        #[allow(non_snake_case)]
        let Const = |v: &str| TypeInfo::from(VariableType::Const(Rc::from(v)));
        #[allow(non_snake_case)]
        let Error = |error: String| TypeInfo {
            kind: Rc::from(VariableType::Any),
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
                if let Some(f) = from {
                    let from_type = self.determine(f, scope.clone());
                    if !from_type.satisfies(&VariableType::Number) {
                        self.set_error(node, format!("Invalid slice index: expected a `number`, but found `{from_type}`."));
                    }
                }

                if let Some(t) = to {
                    let to_type = self.determine(t, scope.clone());
                    if !to_type.satisfies(&VariableType::Number) {
                        self.set_error(
                            node,
                            format!(
                                "Invalid slice index: expected a `number`, but found `{to_type}`."
                            ),
                        );
                    }
                }

                let node_type = self.determine(node, scope.clone());
                match node_type.kind.widen() {
                    VariableType::Any => V(VariableType::Any),
                    VariableType::Array(inner) => TypeInfo::from(inner.clone()),
                    VariableType::String => V(VariableType::String),
                    _ => Error("Slice operation is only allowed on `string | any[]`".to_string()),
                }
            }

            Node::Array(items) => {
                let mut type_list: Vec<Rc<VariableType>> = items
                    .iter()
                    .map(|n| self.determine(n, scope.clone()).kind)
                    .collect();
                let first = type_list.pop();
                let all_same = type_list.iter().all(|t| Some(t) == first.as_ref());

                match (first, all_same) {
                    (Some(typ), true) => V(VariableType::Array(typ)),
                    _ => V(VariableType::Array(Rc::new(VariableType::Any))),
                }
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

                V(VariableType::Object(obj_type))
            }
            Node::Identifier(i) => TypeInfo::from(scope.root_data.get(i)),
            Node::Member { node, property } => {
                let node_type = self.determine(node, scope.clone());
                let property_type = self.determine(property, scope.clone());

                match node_type.kind.as_ref() {
                    VariableType::Any => V(VariableType::Any),
                    VariableType::Null => V(VariableType::Null),
                    VariableType::Array(inner) => {
                        if !property_type.satisfies(&VariableType::Number) {
                            self.set_error(
                                property,
                                format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                            );
                        }

                        TypeInfo::from(inner.clone())
                    }
                    VariableType::Object(obj) => {
                        if !property_type.satisfies(&VariableType::String) {
                            self.set_error(
                                property,
                                format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                            );
                        }

                        match property_type.as_const_str() {
                            None => V(VariableType::Any),
                            Some(key) => TypeInfo::from(
                                obj.get(&key).cloned().unwrap_or(Rc::new(VariableType::Any)),
                            ),
                        }
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
                        LogicalOperator::NullishCoalescing => TypeInfo::from(right_type.kind),
                    },
                    Operator::Comparison(comp) => match comp {
                        ComparisonOperator::Equal => {
                            if !left_type.satisfies(&right_type) && !left_type.is_null() && !right_type.is_null() {
                                on_fly_error.replace(format!(
                                    "Hint: Expression will always evaluate to `false` because `{left_type}` != `{right_type}`."
                                ));
                            }

                            V(VariableType::Bool)
                        },
                        ComparisonOperator::NotEqual => {
                            if !left_type.satisfies(&right_type) && !left_type.is_null() && !right_type.is_null() {
                                on_fly_error.replace(format!(
                                    "Hint: Expression will always evaluate to `true` because `{left_type}` != `{right_type}`."
                                ));
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
                                if !left_type.satisfies(&inner_type) {
                                    let expected = match comp {
                                        ComparisonOperator::In => "false",
                                        _ => "true"
                                    };

                                    on_fly_error.replace(format!(
                                        "Hint: Expression will always evaluate to `{expected}`. because array contains element of type `{inner_type}`, and `{left_type}` != `{inner_type}`."
                                    ));
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

                V(true_type.kind.merge(false_type.kind.as_ref()))
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
                    | Operator::QuestionMark => Error("Unsupported operator".to_string()),
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
                let mut type_list: Vec<Rc<VariableType>> = arguments
                    .iter()
                    .map(|n| self.determine(n, scope.clone()).kind)
                    .collect();

                if let FunctionKind::Closure(_) = kind {
                    let ptr_type = type_list[0].iterator().unwrap_or_default();
                    let new_type = self.determine(
                        arguments[1],
                        IntelliSenseScope {
                            pointer_data: &ptr_type,
                            current_data: scope.current_data,
                            root_data: scope.root_data,
                        },
                    );

                    type_list[1] = new_type.kind;
                }

                match kind {
                    FunctionKind::Internal(_) | FunctionKind::Deprecated(_) => {
                        let Some(def) = FunctionRegistry::get_definition(kind) else {
                            return V(VariableType::Any);
                        };

                        let typecheck = def.check_types(type_list.as_slice());
                        for (i, arg_error) in typecheck.arguments {
                            self.set_error(arguments[i], arg_error);
                        }

                        TypeInfo {
                            kind: Rc::new(typecheck.return_type),
                            error: typecheck.general,
                        }
                    }
                    FunctionKind::Closure(c) => {
                        if !type_list[0].is_iterable() {
                            self.set_error(
                                arguments[0],
                                format!("Argument of type `{}` is not `iterable`.", type_list[0]),
                            );
                        }

                        // Boolean callbacks
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
                            ClosureFunction::Map => V(VariableType::Array(type_list[1].clone())),
                            ClosureFunction::FlatMap => V(VariableType::Any),
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
                let type_list: Vec<Rc<VariableType>> = once(this_type.kind)
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
                    kind: Rc::new(typecheck.return_type),
                    error: typecheck.general,
                }
            }
            Node::Closure(c) => self.determine(c, scope.clone()),
            Node::Parenthesized(c) => self.determine(c, scope.clone()),
            Node::Error { node, error } => match node {
                None => TypeInfo {
                    kind: Rc::new(VariableType::Any),
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

#[allow(unused)]
fn node_address(node: &Node) -> usize {
    node as *const Node as usize
}
