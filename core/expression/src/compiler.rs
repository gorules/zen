use std::cell::UnsafeCell;
use std::rc::Rc;

use bumpalo::Bump;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use thiserror::Error;

use crate::ast::Node;

use crate::compiler::CompilerError::{
    ArgumentNotFound, UnknownBinaryOperator, UnknownBuiltIn, UnknownUnaryOperator,
};
use crate::opcodes::{Opcode, TypeCheckKind, TypeConversionKind, Variable};

type Bytecode<'a> = Rc<UnsafeCell<Vec<&'a Opcode<'a>>>>;

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("Unknown unary operator: {operator}")]
    UnknownUnaryOperator { operator: String },

    #[error("Unknown binary operator: {operator}")]
    UnknownBinaryOperator { operator: String },

    #[error("Unknown builtin: {builtin}")]
    UnknownBuiltIn { builtin: String },

    #[error("Argument not found for builtin {builtin} at index {index}")]
    ArgumentNotFound { builtin: String, index: usize },
}

pub struct Compiler<'a> {
    root: &'a Node<'a>,
    bytecode: Bytecode<'a>,
    bump: &'a Bump,
}

impl<'a> Compiler<'a> {
    pub fn new(root: &'a Node<'a>, bytecode: Bytecode<'a>, bump: &'a Bump) -> Self {
        Self {
            root,
            bytecode,
            bump,
        }
    }

    pub fn compile(&self) -> Result<(), CompilerError> {
        self.compile_node(self.root)?;
        Ok(())
    }

    fn emit(&self, op: Opcode<'a>) -> usize {
        let bc = unsafe { &mut *self.bytecode.get() };
        bc.push(self.bump.alloc(op));
        bc.len()
    }

    fn emit_loop<F>(&self, mut body: F) -> Result<(), CompilerError>
    where
        F: FnMut() -> Result<(), CompilerError>,
    {
        let begin = unsafe { (*self.bytecode.get()).len() };
        let end = self.emit(Opcode::JumpIfEnd(0));

        body()?;

        self.emit(Opcode::IncrementIt);
        let e = self.emit(Opcode::JumpBackward(self.calc_backward_jump(begin)));
        self.replace(end, Opcode::JumpIfEnd(e - end));
        Ok(())
    }

    fn emit_cond<F>(&self, mut body: F)
    where
        F: FnMut(),
    {
        let noop = self.emit(Opcode::JumpIfFalse(0));
        self.emit(Opcode::Pop);

        body();

        let jmp = self.emit(Opcode::Jump(0));
        self.replace(noop, Opcode::JumpIfFalse(jmp - noop));
        let e = self.emit(Opcode::Pop);
        self.replace(jmp, Opcode::Jump(e - jmp));
    }

    fn replace(&self, at: usize, op: Opcode<'a>) {
        let bytecode = unsafe { &mut *self.bytecode.get() };
        let _ = std::mem::replace(&mut bytecode[at - 1], self.bump.alloc(op));
    }

    fn calc_backward_jump(&self, to: usize) -> usize {
        unsafe { (*self.bytecode.get()).len() + 1 - to }
    }

    fn compile_argument(
        &self,
        name: &str,
        arguments: &&[&'a Node<'a>],
        index: usize,
    ) -> Result<usize, CompilerError> {
        let arg = arguments.get(index).ok_or_else(|| ArgumentNotFound {
            index,
            builtin: name.to_string(),
        })?;

        self.compile_node(arg)
    }

    fn compile_node(&self, node: &'a Node<'a>) -> Result<usize, CompilerError> {
        match node {
            Node::Null => Ok(self.emit(Opcode::Push(Variable::Null))),
            Node::Bool(v) => Ok(self.emit(Opcode::Push(Variable::Bool(*v)))),
            Node::Number(v) => Ok(self.emit(Opcode::Push(Variable::Number(*v)))),
            Node::String(v) => Ok(self.emit(Opcode::Push(Variable::String(v)))),
            Node::Pointer => Ok(self.emit(Opcode::Pointer)),
            Node::Array(v) => {
                v.iter()
                    .try_for_each(|&n| self.compile_node(n).map(|_| ()))?;
                self.emit(Opcode::Push(Variable::Number(Decimal::from(v.len()))));
                Ok(self.emit(Opcode::Array))
            }
            Node::Identifier(v) => Ok(self.emit(Opcode::FetchEnv(v))),
            Node::Closure(v) => self.compile_node(v),
            Node::Member { node, property } => {
                self.compile_node(node)?;
                self.compile_node(property)?;
                Ok(self.emit(Opcode::Fetch))
            }
            Node::Slice { node, to, from } => {
                self.compile_node(node)?;
                if let Some(t) = to {
                    self.compile_node(t)?;
                } else {
                    self.emit(Opcode::Len);
                    self.emit(Opcode::Push(Variable::Number(dec!(1))));
                    self.emit(Opcode::Subtract);
                }

                if let Some(f) = from {
                    self.compile_node(f)?;
                } else {
                    self.emit(Opcode::Push(Variable::Number(dec!(0))));
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
                    left_bracket,
                    right_bracket,
                }))
            }
            Node::Conditional {
                condition,
                on_true,
                on_false,
            } => {
                self.compile_node(condition)?;
                let otherwise = self.emit(Opcode::JumpIfFalse(0));

                self.emit(Opcode::Pop);
                self.compile_node(on_true)?;
                let end = self.emit(Opcode::Jump(0));

                self.replace(otherwise, Opcode::JumpIfFalse(end - otherwise));
                self.emit(Opcode::Pop);
                let b = self.compile_node(on_false)?;
                self.replace(end, Opcode::Jump(b - end));

                Ok(b)
            }
            Node::Unary { node, operator } => {
                let curr = self.compile_node(node)?;
                match *operator {
                    "+" => Ok(curr),
                    "!" | "not" => Ok(self.emit(Opcode::Not)),
                    "-" => Ok(self.emit(Opcode::Negate)),
                    _ => Err(UnknownUnaryOperator {
                        operator: operator.to_string(),
                    }),
                }
            }
            Node::Binary {
                left,
                right,
                operator,
            } => match *operator {
                "==" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;

                    Ok(self.emit(Opcode::Equal))
                }
                "!=" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;

                    self.emit(Opcode::Equal);
                    Ok(self.emit(Opcode::Not))
                }
                "or" => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::JumpIfTrue(0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::JumpIfTrue(r - end));

                    Ok(r)
                }
                "and" => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::JumpIfFalse(0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::JumpIfFalse(r - end));

                    Ok(r)
                }
                "in" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::In))
                }
                "not in" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    self.emit(Opcode::In);
                    Ok(self.emit(Opcode::Not))
                }
                "<" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Less))
                }
                "<=" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::LessOrEqual))
                }
                ">" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::More))
                }
                ">=" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::MoreOrEqual))
                }
                "+" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Add))
                }
                "-" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Subtract))
                }
                "*" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Multiply))
                }
                "/" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Divide))
                }
                "%" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Modulo))
                }
                "^" => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::Exponent))
                }
                _ => Err(UnknownBinaryOperator {
                    operator: operator.to_string(),
                }),
            },
            Node::BuiltIn { name, arguments } => match *name {
                "len" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Len);
                    self.emit(Opcode::Rot);
                    Ok(self.emit(Opcode::Pop))
                }
                "date" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::ParseDateTime))
                }
                "time" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::ParseTime))
                }
                "duration" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::ParseDuration))
                }
                "startsWith" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.compile_argument(name, arguments, 1)?;
                    Ok(self.emit(Opcode::StartsWith))
                }
                "endsWith" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.compile_argument(name, arguments, 1)?;
                    Ok(self.emit(Opcode::EndsWith))
                }
                "contains" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.compile_argument(name, arguments, 1)?;
                    Ok(self.emit(Opcode::Contains))
                }
                "matches" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.compile_argument(name, arguments, 1)?;
                    Ok(self.emit(Opcode::Matches))
                }
                "extract" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.compile_argument(name, arguments, 1)?;
                    Ok(self.emit(Opcode::Extract))
                }
                "flatten" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Flatten))
                }
                "upper" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Uppercase))
                }
                "lower" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Lowercase))
                }
                "abs" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Abs))
                }
                "avg" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Average))
                }
                "median" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Median))
                }
                "mode" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Mode))
                }
                "max" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Max))
                }
                "min" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Min))
                }
                "sum" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Sum))
                }
                "floor" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Floor))
                }
                "ceil" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Ceil))
                }
                "round" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Round))
                }
                "rand" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::Random))
                }
                "string" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeConversion(TypeConversionKind::String)))
                }
                "number" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeConversion(TypeConversionKind::Number)))
                }
                "isNumeric" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeCheck(TypeCheckKind::Numeric)))
                }
                "startOf" | "endOf" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.compile_argument(name, arguments, 1)?;
                    Ok(self.emit(Opcode::DateFunction(name)))
                }
                "dayOfWeek" | "dayOfMonth" | "dayOfYear" | "weekOfYear" | "monthOfYear"
                | "monthString" | "weekdayString" | "year" | "dateString" => {
                    self.compile_argument(name, arguments, 0)?;
                    Ok(self.emit(Opcode::DateManipulation(name)))
                }
                "all" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    let mut loop_break: usize = 0;
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        loop_break = self.emit(Opcode::JumpIfFalse(0));
                        self.emit(Opcode::Pop);
                        Ok(())
                    })?;
                    let e = self.emit(Opcode::Push(Variable::Bool(true)));
                    self.replace(loop_break, Opcode::JumpIfFalse(e - loop_break));
                    Ok(self.emit(Opcode::End))
                }
                "none" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    let mut loop_break: usize = 0;
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        self.emit(Opcode::Not);
                        loop_break = self.emit(Opcode::JumpIfFalse(0));
                        self.emit(Opcode::Pop);
                        Ok(())
                    })?;
                    let e = self.emit(Opcode::Push(Variable::Bool(true)));
                    self.replace(loop_break, Opcode::JumpIfFalse(e - loop_break));
                    Ok(self.emit(Opcode::End))
                }
                "some" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    let mut loop_break: usize = 0;
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        loop_break = self.emit(Opcode::JumpIfTrue(0));
                        self.emit(Opcode::Pop);
                        Ok(())
                    })?;
                    let e = self.emit(Opcode::Push(Variable::Bool(false)));
                    self.replace(loop_break, Opcode::JumpIfTrue(e - loop_break));
                    Ok(self.emit(Opcode::End))
                }
                "one" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        self.emit_cond(|| {
                            self.emit(Opcode::IncrementCount);
                        });
                        Ok(())
                    })?;
                    self.emit(Opcode::GetCount);
                    self.emit(Opcode::Push(Variable::Number(dec!(1))));
                    self.emit(Opcode::Equal);
                    Ok(self.emit(Opcode::End))
                }
                "filter" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        self.emit_cond(|| {
                            self.emit(Opcode::IncrementCount);
                            self.emit(Opcode::Pointer);
                        });
                        Ok(())
                    })?;
                    self.emit(Opcode::GetCount);
                    self.emit(Opcode::End);
                    Ok(self.emit(Opcode::Array))
                }
                "map" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        Ok(())
                    })?;
                    self.emit(Opcode::GetLen);
                    self.emit(Opcode::End);
                    Ok(self.emit(Opcode::Array))
                }
                "flatMap" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        Ok(())
                    })?;
                    self.emit(Opcode::GetLen);
                    self.emit(Opcode::End);
                    self.emit(Opcode::Array);
                    Ok(self.emit(Opcode::Flatten))
                }
                "count" => {
                    self.compile_argument(name, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    self.emit_loop(|| {
                        self.compile_argument(name, arguments, 1)?;
                        self.emit_cond(|| {
                            self.emit(Opcode::IncrementCount);
                        });
                        Ok(())
                    })?;
                    self.emit(Opcode::GetCount);
                    Ok(self.emit(Opcode::End))
                }
                _ => Err(UnknownBuiltIn {
                    builtin: name.to_string(),
                }),
            },
        }
    }
}
