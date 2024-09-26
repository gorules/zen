use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::rc::Rc;

use crate::compiler::error::{CompilerError, CompilerResult};
use crate::compiler::{Opcode, TypeCheckKind, TypeConversionKind};
use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use crate::parser::{BuiltInFunction, Node};
use crate::variable::Variable;

#[derive(Debug)]
pub struct Compiler<'arena> {
    bytecode: Vec<Opcode<'arena>>,
}

impl<'arena> Compiler<'arena> {
    pub fn new() -> Self {
        Self {
            bytecode: Default::default(),
        }
    }

    pub fn compile(&mut self, root: &'arena Node<'arena>) -> CompilerResult<&[Opcode<'arena>]> {
        self.bytecode.clear();

        CompilerInner::new(&mut self.bytecode, root).compile()?;
        Ok(self.bytecode.as_slice())
    }
}

#[derive(Debug)]
struct CompilerInner<'arena, 'bytecode_ref> {
    root: &'arena Node<'arena>,
    bytecode: &'bytecode_ref mut Vec<Opcode<'arena>>,
}

impl<'arena, 'bytecode_ref> CompilerInner<'arena, 'bytecode_ref> {
    pub fn new(
        bytecode: &'bytecode_ref mut Vec<Opcode<'arena>>,
        root: &'arena Node<'arena>,
    ) -> Self {
        Self { root, bytecode }
    }

    pub fn compile(&mut self) -> CompilerResult<()> {
        self.compile_node(self.root)?;
        Ok(())
    }

    fn emit(&mut self, op: Opcode<'arena>) -> usize {
        self.bytecode.push(op);
        self.bytecode.len()
    }

    fn emit_loop<F>(&mut self, body: F) -> CompilerResult<()>
    where
        F: FnOnce(&mut Self) -> CompilerResult<()>,
    {
        let begin = self.bytecode.len();
        let end = self.emit(Opcode::JumpIfEnd(0));

        body(self)?;

        self.emit(Opcode::IncrementIt);
        let e = self.emit(Opcode::JumpBackward(self.calc_backward_jump(begin)));
        self.replace(end, Opcode::JumpIfEnd(e - end));
        Ok(())
    }

    fn emit_cond<F>(&mut self, mut body: F)
    where
        F: FnMut(&mut Self),
    {
        let noop = self.emit(Opcode::JumpIfFalse(0));
        self.emit(Opcode::Pop);

        body(self);

        let jmp = self.emit(Opcode::Jump(0));
        self.replace(noop, Opcode::JumpIfFalse(jmp - noop));
        let e = self.emit(Opcode::Pop);
        self.replace(jmp, Opcode::Jump(e - jmp));
    }

    fn replace(&mut self, at: usize, op: Opcode<'arena>) {
        let _ = std::mem::replace(&mut self.bytecode[at - 1], op);
    }

    fn calc_backward_jump(&self, to: usize) -> usize {
        self.bytecode.len() + 1 - to
    }

    fn compile_argument(
        &mut self,
        builtin: &BuiltInFunction,
        arguments: &[&'arena Node<'arena>],
        index: usize,
    ) -> CompilerResult<usize> {
        let arg = arguments
            .get(index)
            .ok_or_else(|| CompilerError::ArgumentNotFound {
                index,
                builtin: builtin.to_string(),
            })?;

        self.compile_node(arg)
    }

    fn compile_node(&mut self, node: &'arena Node<'arena>) -> CompilerResult<usize> {
        match node {
            Node::Null => Ok(self.emit(Opcode::Push(Variable::Null))),
            Node::Bool(v) => Ok(self.emit(Opcode::Push(Variable::Bool(*v)))),
            Node::Number(v) => Ok(self.emit(Opcode::Push(Variable::Number(*v)))),
            Node::String(v) => Ok(self.emit(Opcode::Push(Variable::String(Rc::from(*v))))),
            Node::Pointer => Ok(self.emit(Opcode::Pointer)),
            Node::Root => Ok(self.emit(Opcode::FetchRootEnv)),
            Node::Array(v) => {
                v.iter()
                    .try_for_each(|&n| self.compile_node(n).map(|_| ()))?;
                self.emit(Opcode::Push(Variable::Number(Decimal::from(v.len()))));
                Ok(self.emit(Opcode::Array))
            }
            Node::Object(v) => {
                v.iter().try_for_each(|&(key, value)| {
                    self.compile_node(key).map(|_| ())?;
                    self.emit(Opcode::TypeConversion(TypeConversionKind::String));
                    self.compile_node(value).map(|_| ())?;
                    Ok(())
                })?;

                self.emit(Opcode::Push(Variable::Number(Decimal::from(v.len()))));
                Ok(self.emit(Opcode::Object))
            }
            Node::Identifier(v) => Ok(self.emit(Opcode::FetchEnv(v))),
            Node::Closure(v) => self.compile_node(v),
            Node::Parenthesized(v) => self.compile_node(v),
            Node::Member { node, property } => {
                self.compile_node(node)?;
                self.compile_node(property)?;
                Ok(self.emit(Opcode::Fetch))
            }
            Node::TemplateString(parts) => {
                parts.iter().try_for_each(|&n| {
                    self.compile_node(n).map(|_| ())?;
                    self.emit(Opcode::TypeConversion(TypeConversionKind::String));
                    Ok(())
                })?;

                self.emit(Opcode::Push(Variable::Number(Decimal::from(parts.len()))));
                self.emit(Opcode::Array);
                self.emit(Opcode::Push(Variable::String(Rc::from(""))));
                Ok(self.emit(Opcode::Join))
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
                    let end = self.emit(Opcode::JumpIfTrue(0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::JumpIfTrue(r - end));

                    Ok(r)
                }
                Operator::Logical(LogicalOperator::And) => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::JumpIfFalse(0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::JumpIfFalse(r - end));

                    Ok(r)
                }
                Operator::Logical(LogicalOperator::NullishCoalescing) => {
                    self.compile_node(left)?;
                    let end = self.emit(Opcode::JumpIfNotNull(0));
                    self.emit(Opcode::Pop);
                    let r = self.compile_node(right)?;
                    self.replace(end, Opcode::JumpIfNotNull(r - end));

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
                    Ok(self.emit(Opcode::Less))
                }
                Operator::Comparison(ComparisonOperator::LessThanOrEqual) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::LessOrEqual))
                }
                Operator::Comparison(ComparisonOperator::GreaterThan) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::More))
                }
                Operator::Comparison(ComparisonOperator::GreaterThanOrEqual) => {
                    self.compile_node(left)?;
                    self.compile_node(right)?;
                    Ok(self.emit(Opcode::MoreOrEqual))
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
            Node::BuiltIn { kind, arguments } => match kind {
                BuiltInFunction::Len => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.emit(Opcode::Len);
                    self.emit(Opcode::Rot);
                    Ok(self.emit(Opcode::Pop))
                }
                BuiltInFunction::Date => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::ParseDateTime))
                }
                BuiltInFunction::Time => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::ParseTime))
                }
                BuiltInFunction::Duration => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::ParseDuration))
                }
                BuiltInFunction::StartsWith => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::StartsWith))
                }
                BuiltInFunction::EndsWith => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::EndsWith))
                }
                BuiltInFunction::Contains => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::Contains))
                }
                BuiltInFunction::Matches => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::Matches))
                }
                BuiltInFunction::FuzzyMatch => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::FuzzyMatch))
                }
                BuiltInFunction::Split => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;

                    Ok(self.emit(Opcode::Split))
                }
                BuiltInFunction::Extract => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::Extract))
                }
                BuiltInFunction::Flatten => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Flatten))
                }
                BuiltInFunction::Upper => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Uppercase))
                }
                BuiltInFunction::Lower => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Lowercase))
                }
                BuiltInFunction::Abs => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Abs))
                }
                BuiltInFunction::Avg => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Average))
                }
                BuiltInFunction::Median => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Median))
                }
                BuiltInFunction::Mode => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Mode))
                }
                BuiltInFunction::Max => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Max))
                }
                BuiltInFunction::Min => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Min))
                }
                BuiltInFunction::Sum => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Sum))
                }
                BuiltInFunction::Floor => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Floor))
                }
                BuiltInFunction::Ceil => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Ceil))
                }
                BuiltInFunction::Round => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Round))
                }
                BuiltInFunction::Rand => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Random))
                }
                BuiltInFunction::String => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeConversion(TypeConversionKind::String)))
                }
                BuiltInFunction::Number => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeConversion(TypeConversionKind::Number)))
                }
                BuiltInFunction::Bool => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeConversion(TypeConversionKind::Bool)))
                }
                BuiltInFunction::IsNumeric => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::TypeCheck(TypeCheckKind::Numeric)))
                }
                BuiltInFunction::Type => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::GetType))
                }
                BuiltInFunction::Keys => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Keys))
                }
                BuiltInFunction::Values => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::Values))
                }
                BuiltInFunction::StartOf | BuiltInFunction::EndOf => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.compile_argument(kind, arguments, 1)?;
                    Ok(self.emit(Opcode::DateFunction(kind.into())))
                }
                BuiltInFunction::DayOfWeek
                | BuiltInFunction::DayOfMonth
                | BuiltInFunction::DayOfYear
                | BuiltInFunction::WeekOfYear
                | BuiltInFunction::MonthOfYear
                | BuiltInFunction::MonthString
                | BuiltInFunction::WeekdayString
                | BuiltInFunction::Year
                | BuiltInFunction::DateString => {
                    self.compile_argument(kind, arguments, 0)?;
                    Ok(self.emit(Opcode::DateManipulation(kind.into())))
                }
                BuiltInFunction::All => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    let mut loop_break: usize = 0;
                    self.emit_loop(|c| {
                        c.compile_argument(kind, arguments, 1)?;
                        loop_break = c.emit(Opcode::JumpIfFalse(0));
                        c.emit(Opcode::Pop);
                        Ok(())
                    })?;
                    let e = self.emit(Opcode::Push(Variable::Bool(true)));
                    self.replace(loop_break, Opcode::JumpIfFalse(e - loop_break));
                    Ok(self.emit(Opcode::End))
                }
                BuiltInFunction::None => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    let mut loop_break: usize = 0;
                    self.emit_loop(|c| {
                        c.compile_argument(kind, arguments, 1)?;
                        c.emit(Opcode::Not);
                        loop_break = c.emit(Opcode::JumpIfFalse(0));
                        c.emit(Opcode::Pop);
                        Ok(())
                    })?;
                    let e = self.emit(Opcode::Push(Variable::Bool(true)));
                    self.replace(loop_break, Opcode::JumpIfFalse(e - loop_break));
                    Ok(self.emit(Opcode::End))
                }
                BuiltInFunction::Some => {
                    self.compile_argument(kind, arguments, 0)?;
                    self.emit(Opcode::Begin);
                    let mut loop_break: usize = 0;
                    self.emit_loop(|c| {
                        c.compile_argument(kind, arguments, 1)?;
                        loop_break = c.emit(Opcode::JumpIfTrue(0));
                        c.emit(Opcode::Pop);
                        Ok(())
                    })?;
                    let e = self.emit(Opcode::Push(Variable::Bool(false)));
                    self.replace(loop_break, Opcode::JumpIfTrue(e - loop_break));
                    Ok(self.emit(Opcode::End))
                }
                BuiltInFunction::One => {
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
                    self.emit(Opcode::Push(Variable::Number(dec!(1))));
                    self.emit(Opcode::Equal);
                    Ok(self.emit(Opcode::End))
                }
                BuiltInFunction::Filter => {
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
                BuiltInFunction::Map => {
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
                BuiltInFunction::FlatMap => {
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
                BuiltInFunction::Count => {
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
            Node::Error { .. } => Err(CompilerError::UnexpectedErrorNode),
        }
    }
}
