use crate::compiler::error::{CompilerError, CompilerResult};
use crate::compiler::opcode::{FetchFastTarget, Jump};
use crate::compiler::{Compare, Opcode};
use crate::functions::registry::FunctionRegistry;
use crate::functions::{ClosureFunction, FunctionKind, InternalFunction, MethodRegistry};
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::Node;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;

#[derive(Debug)]
pub struct Compiler {
    bytecode: Vec<Opcode>,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            bytecode: Default::default(),
        }
    }

    pub fn compile(&mut self, root: &Node) -> CompilerResult<&[Opcode]> {
        self.bytecode.clear();

        CompilerInner::new(&mut self.bytecode, root).compile()?;
        Ok(self.bytecode.as_slice())
    }

    pub fn get_bytecode(&self) -> &[Opcode] {
        self.bytecode.as_slice()
    }
}

#[derive(Debug)]
struct CompilerInner<'arena, 'bytecode_ref> {
    root: &'arena Node<'arena>,
    bytecode: &'bytecode_ref mut Vec<Opcode>,
}

impl<'arena, 'bytecode_ref> CompilerInner<'arena, 'bytecode_ref> {
    pub fn new(bytecode: &'bytecode_ref mut Vec<Opcode>, root: &'arena Node<'arena>) -> Self {
        Self { root, bytecode }
    }

    pub fn compile(&mut self) -> CompilerResult<()> {
        self.compile_node(self.root)?;
        Ok(())
    }

    fn emit(&mut self, op: Opcode) -> usize {
        self.bytecode.push(op);
        self.bytecode.len()
    }

    fn emit_loop<F>(&mut self, body: F) -> CompilerResult<()>
    where
        F: FnOnce(&mut Self) -> CompilerResult<()>,
    {
        let begin = self.bytecode.len();
        let end = self.emit(Opcode::Jump(Jump::IfEnd, 0));

        body(self)?;

        self.emit(Opcode::IncrementIt);
        let e = self.emit(Opcode::Jump(
            Jump::Backward,
            self.calc_backward_jump(begin) as u32,
        ));
        self.replace(end, Opcode::Jump(Jump::IfEnd, (e - end) as u32));
        Ok(())
    }

    fn emit_cond<F>(&mut self, mut body: F)
    where
        F: FnMut(&mut Self),
    {
        let noop = self.emit(Opcode::Jump(Jump::IfFalse, 0));
        self.emit(Opcode::Pop);

        body(self);

        let jmp = self.emit(Opcode::Jump(Jump::Forward, 0));
        self.replace(noop, Opcode::Jump(Jump::IfFalse, (jmp - noop) as u32));
        let e = self.emit(Opcode::Pop);
        self.replace(jmp, Opcode::Jump(Jump::Forward, (e - jmp) as u32));
    }

    fn replace(&mut self, at: usize, op: Opcode) {
        let _ = std::mem::replace(&mut self.bytecode[at - 1], op);
    }

    fn calc_backward_jump(&self, to: usize) -> usize {
        self.bytecode.len() + 1 - to
    }

    fn compile_argument<T: ToString>(
        &mut self,
        function_kind: T,
        arguments: &[&'arena Node<'arena>],
        index: usize,
    ) -> CompilerResult<usize> {
        let arg = arguments
            .get(index)
            .ok_or_else(|| CompilerError::ArgumentNotFound {
                index,
                function: function_kind.to_string(),
            })?;

        self.compile_node(arg)
    }

    #[cfg_attr(feature = "stack-protection", recursive::recursive)]
    fn compile_member_fast(&mut self, node: &'arena Node<'arena>) -> Option<Vec<FetchFastTarget>> {
        match node {
            Node::Root => Some(vec![FetchFastTarget::Root]),
            Node::Identifier(v) => Some(vec![
                FetchFastTarget::Root,
                FetchFastTarget::String(Arc::from(*v)),
            ]),
            Node::Member { node, property } => {
                let mut path = self.compile_member_fast(node)?;
                match property {
                    Node::String(v) => {
                        path.push(FetchFastTarget::String(Arc::from(*v)));
                        Some(path)
                    }
                    Node::Number(v) => {
                        if let Some(idx) = v.to_u32() {
                            path.push(FetchFastTarget::Number(idx));
                            Some(path)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    #[cfg_attr(feature = "stack-protection", recursive::recursive)]
    fn compile_node(&mut self, node: &'arena Node<'arena>) -> CompilerResult<usize> {
        match node {
            Node::Null => Ok(self.emit(Opcode::PushNull)),
            Node::Bool(v) => Ok(self.emit(Opcode::PushBool(*v))),
            Node::Number(v) => Ok(self.emit(Opcode::PushNumber(*v))),
            Node::String(v) => Ok(self.emit(Opcode::PushString(Arc::from(*v)))),
            Node::Pointer => Ok(self.emit(Opcode::Pointer)),
            Node::Root => Ok(self.emit(Opcode::FetchRootEnv)),
            Node::Array(v) => {
                v.iter()
                    .try_for_each(|&n| self.compile_node(n).map(|_| ()))?;
                self.emit(Opcode::PushNumber(Decimal::from(v.len())));
                Ok(self.emit(Opcode::Array))
            }
            Node::Object(v) => {
                v.iter().try_for_each(|&(key, value)| {
                    self.compile_node(key).map(|_| ())?;
                    self.emit(Opcode::CallFunction {
                        arg_count: 1,
                        kind: FunctionKind::Internal(InternalFunction::String),
                    });
                    self.compile_node(value).map(|_| ())?;
                    Ok(())
                })?;

                self.emit(Opcode::PushNumber(Decimal::from(v.len())));
                Ok(self.emit(Opcode::Object))
            }
            Node::Identifier(v) => Ok(self.emit(Opcode::FetchEnv(Arc::from(*v)))),
            Node::Closure(v) => self.compile_node(v),
            Node::Parenthesized(v) => self.compile_node(v),
            Node::Member {
                node: n,
                property: p,
            } => {
                if let Some(path) = self.compile_member_fast(node) {
                    Ok(self.emit(Opcode::FetchFast(path)))
                } else {
                    self.compile_node(n)?;
                    self.compile_node(p)?;
                    Ok(self.emit(Opcode::Fetch))
                }
            }
            Node::TemplateString(parts) => {
                parts.iter().try_for_each(|&n| {
                    self.compile_node(n).map(|_| ())?;
                    self.emit(Opcode::CallFunction {
                        arg_count: 1,
                        kind: FunctionKind::Internal(InternalFunction::String),
                    });
                    Ok(())
                })?;

                self.emit(Opcode::PushNumber(Decimal::from(parts.len())));
                self.emit(Opcode::Array);
                self.emit(Opcode::PushString(Arc::from("")));
                Ok(self.emit(Opcode::Join))
            }
            Node::Slice { node, to, from } => {
                self.compile_node(node)?;
                if let Some(t) = to {
                    self.compile_node(t)?;
                } else {
                    self.emit(Opcode::Len);
                    self.emit(Opcode::PushNumber(dec!(1)));
                    self.emit(Opcode::Subtract);
                }

                if let Some(f) = from {
                    self.compile_node(f)?;
                } else {
                    self.emit(Opcode::PushNumber(dec!(0)));
                }

                Ok(self.emit(Opcode::Slice))
            }
            Node::Interval {
                left,
                right,
                left_bracket,
                right_bracket,
            } => {
                self.compile_node(left)?;
                self.compile_node(right)?;
                Ok(self.emit(Opcode::Interval {
                    left_bracket: *left_bracket,
                    right_bracket: *right_bracket,
                }))
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.compile_node(condition)?;
                let otherwise = self.emit(Opcode::Jump(Jump::IfFalse, 0));

                self.emit(Opcode::Pop);
                self.compile_node(on_true)?;
                let end = self.emit(Opcode::Jump(Jump::Forward, 0));

                self.replace(
                    otherwise,
                    Opcode::Jump(Jump::IfFalse, (end - otherwise) as u32),
                );
                self.emit(Opcode::Pop);
                let b = self.compile_node(on_false)?;
                self.replace(end, Opcode::Jump(Jump::Forward, (b - end) as u32));

                Ok(b)
            }
            Node::Unary { node, operator } => {
                let curr = self.compile_node(node)?;
                match *operator {
                    Operator::Arithmetic(ArithmeticOperator::Add) => Ok(curr),
                    Operator::Arithmetic(ArithmeticOperator::Subtract) => {
                        Ok(self.emit(Opcode::Negate))
                    }
                    Operator::Logical(LogicalOperator::Not) => Ok(self.emit(Opcode::Not)),
                    _ => Err(CompilerError::UnknownUnaryOperator {
                        operator: operator.to_string(),
                    }),
                }
            }
            Node::Binary {
                left,
                right,
                operator,
            } => match *operator {
                Operator::Comparison(ComparisonOperator::Equal) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;

                    Ok(self.emit(Opcode::Equal))
                }
                Operator::Comparison(ComparisonOperator::NotEqual) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;

                    self.emit(Opcode::Equal);
                    Ok(self.emit(Opcode::Not))
                }
                Operator::Logical(LogicalOperator::Or) => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::Jump(Jump::IfTrue, 0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::Jump(Jump::IfTrue, (r - end) as u32));

                    Ok(r)
                }
                Operator::Logical(LogicalOperator::And) => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::Jump(Jump::IfFalse, 0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::Jump(Jump::IfFalse, (r - end) as u32));

                    Ok(r)
                }
                Operator::Logical(LogicalOperator::NullishCoalescing) => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::Jump(Jump::IfNotNull, 0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::Jump(Jump::IfNotNull, (r - end) as u32));

                    Ok(r)
                }
                Operator::Comparison(ComparisonOperator::In) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::In))
                }
                Operator::Comparison(ComparisonOperator::NotIn) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    self.emit(Opcode::In);
                    Ok(self.emit(Opcode::Not))
                }
                Operator::Comparison(ComparisonOperator::LessThan) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Compare(Compare::Less)))
                }
                Operator::Comparison(ComparisonOperator::LessThanOrEqual) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Compare(Compare::LessOrEqual)))
                }
                Operator::Comparison(ComparisonOperator::GreaterThan) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Compare(Compare::More)))
                }
                Operator::Comparison(ComparisonOperator::GreaterThanOrEqual) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Compare(Compare::MoreOrEqual)))
                }
                Operator::Arithmetic(ArithmeticOperator::Add) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Add))
                }
                Operator::Arithmetic(ArithmeticOperator::Subtract) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Subtract))
                }
                Operator::Arithmetic(ArithmeticOperator::Multiply) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Multiply))
                }
                Operator::Arithmetic(ArithmeticOperator::Divide) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Divide))
                }
                Operator::Arithmetic(ArithmeticOperator::Modulus) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Modulo))
                }
                Operator::Arithmetic(ArithmeticOperator::Power) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Exponent))
                }
                _ => Err(CompilerError::UnknownBinaryOperator {
                    operator: operator.to_string(),
                }),
            },
            Node::FunctionCall { kind, arguments } => match kind {
                FunctionKind::Internal(_) | FunctionKind::Deprecated(_) => {
                    let function = FunctionRegistry::get_definition(kind).ok_or_else(|| {
                        CompilerError::UnknownFunction {
                            name: kind.to_string(),
                        }
                    })?;

                    let min_params = function.required_parameters();
                    let max_params = min_params + function.optional_parameters();
                    if arguments.len() < min_params || arguments.len() > max_params {
                        return Err(CompilerError::InvalidFunctionCall {
                            name: kind.to_string(),
                            message: "Invalid number of arguments".to_string(),
                        });
                    }

                    for i in 0..arguments.len() {
                        self.compile_argument(kind, arguments, i)?;
                    }

                    Ok(self.emit(Opcode::CallFunction {
                        kind: kind.clone(),
                        arg_count: arguments.len() as u32,
                    }))
                }
                FunctionKind::Closure(c) => match c {
                    ClosureFunction::All => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        let mut loop_break: usize = 0;
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            loop_break = c.emit(Opcode::Jump(Jump::IfFalse, 0));
                            c.emit(Opcode::Pop);
                            Ok(())
                        })?;
                        let e = self.emit(Opcode::PushBool(true));
                        self.replace(
                            loop_break,
                            Opcode::Jump(Jump::IfFalse, (e - loop_break) as u32),
                        );
                        Ok(self.emit(Opcode::End))
                    }
                    ClosureFunction::None => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        let mut loop_break: usize = 0;
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            c.emit(Opcode::Not);
                            loop_break = c.emit(Opcode::Jump(Jump::IfFalse, 0));
                            c.emit(Opcode::Pop);
                            Ok(())
                        })?;
                        let e = self.emit(Opcode::PushBool(true));
                        self.replace(
                            loop_break,
                            Opcode::Jump(Jump::IfFalse, (e - loop_break) as u32),
                        );
                        Ok(self.emit(Opcode::End))
                    }
                    ClosureFunction::Some => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        let mut loop_break: usize = 0;
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            loop_break = c.emit(Opcode::Jump(Jump::IfTrue, 0));
                            c.emit(Opcode::Pop);
                            Ok(())
                        })?;
                        let e = self.emit(Opcode::PushBool(false));
                        self.replace(
                            loop_break,
                            Opcode::Jump(Jump::IfTrue, (e - loop_break) as u32),
                        );
                        Ok(self.emit(Opcode::End))
                    }
                    ClosureFunction::One => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            c.emit_cond(|c| {
                                c.emit(Opcode::IncrementCount);
                            });
                            Ok(())
                        })?;
                        self.emit(Opcode::GetCount);
                        self.emit(Opcode::PushNumber(dec!(1)));
                        self.emit(Opcode::Equal);
                        Ok(self.emit(Opcode::End))
                    }
                    ClosureFunction::Filter => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            c.emit_cond(|c| {
                                c.emit(Opcode::IncrementCount);
                                c.emit(Opcode::Pointer);
                            });
                            Ok(())
                        })?;
                        self.emit(Opcode::GetCount);
                        self.emit(Opcode::End);
                        Ok(self.emit(Opcode::Array))
                    }
                    ClosureFunction::Map => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            Ok(())
                        })?;
                        self.emit(Opcode::GetLen);
                        self.emit(Opcode::End);
                        Ok(self.emit(Opcode::Array))
                    }
                    ClosureFunction::FlatMap => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            Ok(())
                        })?;
                        self.emit(Opcode::GetLen);
                        self.emit(Opcode::End);
                        self.emit(Opcode::Array);
                        Ok(self.emit(Opcode::Flatten))
                    }
                    ClosureFunction::Count => {
                        self.compile_argument(kind, arguments, 0)?;
                        self.emit(Opcode::Begin);
                        self.emit_loop(|c| {
                            c.compile_argument(kind, arguments, 1)?;
                            c.emit_cond(|c| {
                                c.emit(Opcode::IncrementCount);
                            });
                            Ok(())
                        })?;
                        self.emit(Opcode::GetCount);
                        Ok(self.emit(Opcode::End))
                    }
                },
            },
            Node::MethodCall {
                kind,
                this,
                arguments,
            } => {
                let method = MethodRegistry::get_definition(kind).ok_or_else(|| {
                    CompilerError::UnknownFunction {
                        name: kind.to_string(),
                    }
                })?;

                self.compile_node(this)?;

                let min_params = method.required_parameters() - 1;
                let max_params = min_params + method.optional_parameters();
                if arguments.len() < min_params || arguments.len() > max_params {
                    return Err(CompilerError::InvalidMethodCall {
                        name: kind.to_string(),
                        message: "Invalid number of arguments".to_string(),
                    });
                }

                for i in 0..arguments.len() {
                    self.compile_argument(kind, arguments, i)?;
                }

                Ok(self.emit(Opcode::CallMethod {
                    kind: kind.clone(),
                    arg_count: arguments.len() as u32,
                }))
            }
            Node::Error { .. } => Err(CompilerError::UnexpectedErrorNode),
        }
    }
}
