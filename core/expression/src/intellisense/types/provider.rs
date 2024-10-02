use crate::intellisense::scope::IntelliSenseScope;
use crate::intellisense::types::type_info::TypeInfo;
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::{Arity, BuiltInFunction, Node};
use crate::variable::VariableType;
use serde_json::{Number, Value};
use std::collections::HashMap;
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

        s.determine(root, scope, false);
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

    fn determine(&mut self, node: &Node, scope: IntelliSenseScope, detailed: bool) -> TypeInfo {
        #[allow(non_snake_case)]
        let V = |vt: VariableType| TypeInfo::from(vt);
        #[allow(non_snake_case)]
        let Const = |v: Value| TypeInfo::from(VariableType::Constant(Rc::new(v)));
        #[allow(non_snake_case)]
        let Error = |error: String| TypeInfo {
            kind: Rc::from(VariableType::Any),
            error: Some(error),
        };

        let node_type = match node {
            Node::Null => V(VariableType::Null),
            Node::Bool(b) => match detailed {
                true => Const(Value::Bool(*b)),
                false => V(VariableType::Bool),
            },
            Node::Number(n) => match detailed {
                true => Const(Value::Number(Number::from_string_unchecked(
                    n.normalize().to_string(),
                ))),
                false => V(VariableType::Number),
            },
            Node::String(s) => match detailed {
                true => Const(Value::String(s.to_string())),
                false => V(VariableType::String),
            },
            Node::TemplateString(_) => V(VariableType::String),

            Node::Pointer => V(scope.pointer_data.clone()),
            Node::Root => V(scope.root_data.clone()),

            Node::Slice { node, from, to } => {
                if let Some(f) = from {
                    let from_type = self.determine(f, scope.clone(), false);
                    if !from_type.satisfies(&VariableType::Number) {
                        self.set_error(node, format!("Invalid slice index: expected a `number`, but found `{from_type}`."));
                    }
                }

                if let Some(t) = to {
                    let to_type = self.determine(t, scope.clone(), false);
                    if !to_type.satisfies(&VariableType::Number) {
                        self.set_error(
                            node,
                            format!(
                                "Invalid slice index: expected a `number`, but found `{to_type}`."
                            ),
                        );
                    }
                }

                let node_type = self.determine(node, scope.clone(), false);
                match node_type.kind.as_ref() {
                    VariableType::Any => V(VariableType::Any),
                    VariableType::String => V(VariableType::String),
                    VariableType::Array(inner) => TypeInfo::from(inner.clone()),
                    VariableType::Constant(c) => match c.as_ref() {
                        Value::String(_) => V(VariableType::String),
                        Value::Array(inner) => match VariableType::from(inner).array_item() {
                            Some(item) => TypeInfo::from(item),
                            None => Error("Array expected".to_string()),
                        },
                        _ => {
                            Error("Slice operation is only allowed on `string | any[]`".to_string())
                        }
                    },
                    _ => Error("Slice operation is only allowed on `string | any[]`".to_string()),
                }
            }

            Node::Array(items) => {
                let mut type_list: Vec<Rc<VariableType>> = items
                    .iter()
                    .map(|n| self.determine(n, scope.clone(), false).kind)
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
                        let key_type = self.determine(k, scope.clone(), true);
                        Some((
                            key_type.kind.as_const_str()?.to_string(),
                            self.determine(v, scope.clone(), false).kind,
                        ))
                    })
                    .collect();

                V(VariableType::Object(obj_type))
            }
            Node::Identifier(i) => TypeInfo::from(scope.root_data.get(&VariableType::Constant(
                Rc::from(Value::String(i.to_string())),
            ))),
            Node::Member { node, property } => {
                let node_type = self.determine(node, scope.clone(), true);
                let property_type = self.determine(property, scope.clone(), true);

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
                                obj.get(key).cloned().unwrap_or(Rc::new(VariableType::Any)),
                            ),
                        }
                    }
                    VariableType::Constant(c) => match c.as_ref() {
                        Value::Null => V(VariableType::Null),
                        Value::Array(arr) => {
                            if !property_type.satisfies(&VariableType::Number) {
                                self.set_error(
                                    property,
                                    format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                                );
                            }

                            match VariableType::from(arr).array_item() {
                                Some(item) => TypeInfo::from(item),
                                None => Error("Expected an array".to_string()),
                            }
                        }
                        Value::Object(obj) => {
                            if !property_type.satisfies(&VariableType::String) {
                                self.set_error(
                                    property,
                                    format!("Expression of type `{property_type}` cannot be used to index `{node_type}`."),
                                );
                            }

                            match property_type.as_const_str() {
                                None => V(VariableType::Any),
                                Some(key) => V(obj
                                    .get(key)
                                    .cloned()
                                    .map(VariableType::from)
                                    .unwrap_or(VariableType::Any)),
                            }
                        }
                        _ => Error(format!("Expression of type `{property_type}` cannot be used to index `{node_type}`.")),
                    },
                    _ => Error(format!("Expression of type `{property_type}` cannot be used to index `{node_type}`.")),
                }
            }
            Node::Binary {
                left,
                right,
                operator,
            } => {
                let left_type = self.determine(left, scope.clone(), false);
                let right_type = self.determine(right, scope.clone(), false);

                match operator {
                    Operator::Arithmetic(arith) => match arith {
                        ArithmeticOperator::Add => match (left_type.omit_const(), right_type.omit_const()) {
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
                        | ArithmeticOperator::Power => match (left_type.omit_const(), right_type.omit_const()) {
                            (VariableType::Number | VariableType::Any, VariableType::Number | VariableType::Any) => V(VariableType::Number),
                            _ => Error(format!(
                                "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                            )),
                        },
                    },
                    Operator::Logical(l) => match l {
                        LogicalOperator::And | LogicalOperator::Or | LogicalOperator::Not => {
                            match (left_type.omit_const(), right_type.omit_const()) {
                                (VariableType::Bool | VariableType::Any, VariableType::Bool | VariableType::Any) => V(VariableType::Bool),
                                _ => Error(format!(
                                    "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                                )),
                            }
                        }
                        LogicalOperator::NullishCoalescing => TypeInfo::from(right_type.kind),
                    },
                    Operator::Comparison(comp) => match comp {
                        ComparisonOperator::Equal => V(VariableType::Bool),
                        ComparisonOperator::NotEqual => V(VariableType::Bool),
                        ComparisonOperator::LessThan
                        | ComparisonOperator::GreaterThan
                        | ComparisonOperator::LessThanOrEqual
                        | ComparisonOperator::GreaterThanOrEqual => match (left_type.omit_const(), right_type.omit_const()) {
                            (VariableType::Number | VariableType::Any, VariableType::Number | VariableType::Any) => V(VariableType::Bool),
                            _ => Error(format!(
                                "Operator `{operator}` cannot be applied to types `{left_type}` and `{right_type}`."
                            )),
                        },
                        ComparisonOperator::In | ComparisonOperator::NotIn => match (left_type.kind.as_ref(), right_type.kind.as_ref()) {
                            (_, VariableType::Array(_)) => V(VariableType::Bool),
                            (_, VariableType::Object(_)) => V(VariableType::Bool),
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
                let condition_type = self.determine(condition, scope.clone(), false);
                if !condition_type.satisfies(&VariableType::Bool) {
                    self.set_error(
                        condition,
                        format!("Ternary operator cannot be applied to type `{condition_type}`."),
                    );
                }

                let true_type = self.determine(on_true, scope.clone(), false);
                let false_type = self.determine(on_false, scope.clone(), false);

                V(true_type.kind.merge(false_type.kind.as_ref()))
            }
            Node::Unary { node, operator } => {
                let node_type = self.determine(node, scope.clone(), false);

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
                let left_type = self.determine(left, scope.clone(), false);
                if !left_type.satisfies(&VariableType::Number) {
                    self.set_error(
                        left,
                        format!("Interval cannot be created from type `{left_type}`."),
                    )
                }

                let right_type = self.determine(right, scope.clone(), false);
                if !right_type.satisfies(&VariableType::Number) {
                    self.set_error(
                        right,
                        format!("Interval cannot be created from type `{right_type}`."),
                    )
                }

                V(VariableType::Any)
            }
            Node::BuiltIn { arguments, kind } => {
                let mut type_list: Vec<Rc<VariableType>> = arguments
                    .iter()
                    .map(|n| self.determine(n, scope.clone(), false).kind)
                    .collect();

                let arg_len = match kind.arity() {
                    Arity::Single => 1,
                    Arity::Closure | Arity::Dual => 2,
                };

                if type_list.len() != arg_len {
                    self.set_type(
                        node,
                        Error(format!(
                            "Expected {arg_len} arguments, but got {}.",
                            type_list.len()
                        )),
                    );
                }

                if kind.arity() == Arity::Closure {
                    let ptr_type = type_list[0].array_item().unwrap_or_default();
                    let new_type = self.determine(
                        arguments[1],
                        IntelliSenseScope {
                            pointer_data: &ptr_type,
                            current_data: scope.current_data,
                            root_data: scope.root_data,
                        },
                        false,
                    );

                    type_list[1] = new_type.kind;
                }

                match kind {
                    BuiltInFunction::Len => {
                        if !type_list[0].satisfies(&VariableType::String)
                            && !type_list[0]
                                .satisfies(&VariableType::Array(VariableType::Any.into()))
                        {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `string | any[]`.", type_list[0]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::Contains => {
                        match (type_list[0].omit_const(), type_list[1].omit_const()) {
                            (VariableType::String, VariableType::String)
                            | (VariableType::Any, _)
                            | (_, VariableType::Any) => {
                                // ok
                            }
                            (VariableType::Array(vt), b) => {
                                if !b.satisfies(&vt) {
                                    self.set_error(arguments[1], format!("Argument of type `{b}` is not assignable to parameter of type `{vt}`."));
                                }
                            }
                            _ => self.set_error(node, "Unsupported call signature.".to_string()),
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::Upper | BuiltInFunction::Lower => {
                        if !type_list[0].satisfies(&VariableType::String) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[0]));
                        }

                        V(VariableType::String)
                    }
                    BuiltInFunction::StartsWith
                    | BuiltInFunction::EndsWith
                    | BuiltInFunction::Matches => {
                        if !type_list[0].satisfies(&VariableType::String) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::String) {
                            self.set_error(arguments[1], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[1]));
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::Extract | BuiltInFunction::Split => {
                        if !type_list[0].satisfies(&VariableType::String) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::String) {
                            self.set_error(arguments[1], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[1]));
                        }

                        V(VariableType::Array(Rc::new(VariableType::String)))
                    }
                    BuiltInFunction::FuzzyMatch => {
                        if !type_list[0].satisfies(&VariableType::String)
                            && !type_list[0]
                                .satisfies(&VariableType::Array(Rc::new(VariableType::String)))
                        {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `string | string[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::String) {
                            self.set_error(arguments[1], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[1]));
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::Abs
                    | BuiltInFunction::Rand
                    | BuiltInFunction::Floor
                    | BuiltInFunction::Ceil
                    | BuiltInFunction::Round => {
                        if !type_list[0].satisfies(&VariableType::Number) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `number`.", type_list[0]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::Sum
                    | BuiltInFunction::Avg
                    | BuiltInFunction::Min
                    | BuiltInFunction::Max
                    | BuiltInFunction::Median
                    | BuiltInFunction::Mode => {
                        if !type_list[0]
                            .satisfies(&VariableType::Array(Rc::new(VariableType::Number)))
                        {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `number[]`.", type_list[0]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::IsNumeric => V(VariableType::Bool),
                    BuiltInFunction::String => V(VariableType::String),
                    BuiltInFunction::Number => V(VariableType::Number),
                    BuiltInFunction::Bool => V(VariableType::Bool),
                    BuiltInFunction::Type => V(VariableType::String),
                    BuiltInFunction::Date => {
                        if !type_list[0].satisfies(&VariableType::Number)
                            && !type_list[0].satisfies(&VariableType::String)
                        {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `number | string`.", type_list[0]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::Time | BuiltInFunction::Duration => {
                        if !type_list[0].satisfies(&VariableType::String) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[0]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::Year
                    | BuiltInFunction::DayOfWeek
                    | BuiltInFunction::DayOfMonth
                    | BuiltInFunction::DayOfYear
                    | BuiltInFunction::WeekOfYear
                    | BuiltInFunction::MonthOfYear => {
                        if !type_list[0].satisfies(&VariableType::Number) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `number`.", type_list[0]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::MonthString
                    | BuiltInFunction::DateString
                    | BuiltInFunction::WeekdayString => {
                        if !type_list[0].satisfies(&VariableType::Number) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `number`.", type_list[0]));
                        }

                        V(VariableType::String)
                    }
                    BuiltInFunction::StartOf | BuiltInFunction::EndOf => {
                        if !type_list[0].satisfies(&VariableType::Number) {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `number`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::String) {
                            self.set_error(arguments[1], format!("Argument of type `{}` is not assignable to parameter of type `string`.", type_list[1]));
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::Keys => {
                        if !type_list[0].satisfies_object() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `object`.", type_list[0]));
                        }

                        V(VariableType::Array(Rc::new(VariableType::String)))
                    }
                    BuiltInFunction::Values => match type_list[0].as_ref() {
                        VariableType::Any | VariableType::Object(_) => {
                            V(VariableType::Array(VariableType::Any.into()))
                        }
                        VariableType::Constant(c) => match c.as_ref() {
                            Value::Object(obj) => {
                                let s: Vec<Value> = obj.values().cloned().collect();
                                V(s.into())
                            }
                            _ => {
                                self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `object`.", type_list[0]));
                                V(VariableType::Array(VariableType::Any.into()))
                            }
                        },
                        _ => {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `object`", type_list[0]));
                            V(VariableType::Array(VariableType::Any.into()))
                        }
                    },
                    BuiltInFunction::All => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::Bool) {
                            self.set_error(
                                arguments[1],
                                format!(
                                    "Callback must return a `bool`, but its return type is `{}`.",
                                    type_list[1]
                                ),
                            );
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::Some => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::Bool) {
                            self.set_error(
                                arguments[1],
                                format!(
                                    "Callback must return a `bool`, but its return type is `{}`.",
                                    type_list[1]
                                ),
                            );
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::None => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::Bool) {
                            self.set_error(
                                arguments[1],
                                format!(
                                    "Callback must return a `bool`, but its return type is `{}`.",
                                    type_list[1]
                                ),
                            );
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::Filter => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::Bool) {
                            self.set_error(
                                arguments[1],
                                format!(
                                    "Callback must return a `bool`, but its return type is `{}`.",
                                    type_list[1]
                                ),
                            );
                        }

                        TypeInfo::from(type_list[0].clone())
                    }
                    BuiltInFunction::Map => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        V(VariableType::Array(type_list[1].clone()))
                    }
                    BuiltInFunction::Count => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::Bool) {
                            self.set_error(
                                arguments[1],
                                format!(
                                    "Callback must return a `bool`, but its return type is `{}`.",
                                    type_list[1]
                                ),
                            );
                        }

                        V(VariableType::Number)
                    }
                    BuiltInFunction::One => {
                        if !type_list[0].satisfies_array() {
                            self.set_error(arguments[0], format!("Argument of type `{}` is not assignable to parameter of type `any[]`.", type_list[0]));
                        }

                        if !type_list[1].satisfies(&VariableType::Bool) {
                            self.set_error(
                                arguments[1],
                                format!(
                                    "Callback must return a `bool`, but its return type is `{}`.",
                                    type_list[1]
                                ),
                            );
                        }

                        V(VariableType::Bool)
                    }
                    BuiltInFunction::FlatMap => V(VariableType::Any),
                    BuiltInFunction::Flatten => V(VariableType::Any),
                }
            }
            Node::Closure(c) => self.determine(c, scope.clone(), false),
            Node::Parenthesized(c) => self.determine(c, scope.clone(), false),
            Node::Error { node, error } => match node {
                None => TypeInfo {
                    kind: Rc::new(VariableType::Any),
                    error: Some(error.to_string()),
                },
                Some(n) => {
                    let typ = self.determine(n, scope.clone(), false);
                    TypeInfo {
                        kind: typ.kind,
                        error: Some(error.to_string()),
                    }
                }
            },
        };

        self.set_type(node, node_type.clone());
        node_type
    }
}

#[allow(unused)]
fn node_address(node: &Node) -> usize {
    node as *const Node as usize
}
