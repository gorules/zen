use crate::compiler::{Opcode, TypeCheckKind, TypeConversionKind};
use crate::variable::Variable;
use crate::variable::Variable::*;
use crate::vm::error::VMError::*;
use crate::vm::error::VMResult;
use crate::vm::helpers::{date_time, date_time_end_of, date_time_start_of, time};
use crate::vm::variable::IntervalObject;
use ahash::{HashMap, HashMapExt};
use chrono::NaiveDateTime;
use chrono::{Datelike, Timelike};
#[cfg(not(feature = "regex-lite"))]
use regex::Regex;
#[cfg(feature = "regex-lite")]
use regex_lite::Regex;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, MathematicalOps};
use rust_decimal_macros::dec;
use std::cell::RefCell;
use std::rc::Rc;
use std::string::String as StdString;

#[derive(Debug)]
pub struct Scope {
    array: Variable,
    len: usize,
    iter: usize,
    count: usize,
}

#[derive(Debug)]
pub struct VM {
    scopes: Vec<Scope>,
    stack: Vec<Variable>,
}

impl VM {
    pub fn new() -> Self {
        Self {
            scopes: Default::default(),
            stack: Default::default(),
        }
    }

    pub fn run(&mut self, bytecode: &[Opcode], env: Variable) -> VMResult<Variable> {
        self.stack.clear();
        self.scopes.clear();

        let s = VMInner::new(bytecode, &mut self.stack, &mut self.scopes).run(env);
        Ok(s?)
    }
}

struct VMInner<'arena, 'parent_ref, 'bytecode_ref> {
    scopes: &'parent_ref mut Vec<Scope>,
    stack: &'parent_ref mut Vec<Variable>,
    bytecode: &'bytecode_ref [Opcode<'arena>],
    ip: usize,
}

impl<'arena, 'parent_ref, 'bytecode_ref> VMInner<'arena, 'parent_ref, 'bytecode_ref> {
    pub fn new(
        bytecode: &'bytecode_ref [Opcode<'arena>],
        stack: &'parent_ref mut Vec<Variable>,
        scopes: &'parent_ref mut Vec<Scope>,
    ) -> Self {
        Self {
            ip: 0,
            scopes,
            stack,
            bytecode,
        }
    }

    fn push(&mut self, var: Variable) {
        self.stack.push(var);
    }

    fn pop(&mut self) -> VMResult<Variable> {
        self.stack.pop().ok_or_else(|| StackOutOfBounds {
            stack: format!("{:?}", self.stack),
        })
    }

    pub fn run(&mut self, env: Variable) -> VMResult<Variable> {
        if self.ip != 0 {
            self.ip = 0;
        }

        while self.ip < self.bytecode.len() {
            let op = self
                .bytecode
                .get(self.ip)
                .ok_or_else(|| OpcodeOutOfBounds {
                    bytecode: format!("{:?}", self.bytecode),
                    index: self.ip,
                })?;

            self.ip += 1;

            match op {
                Opcode::Push(v) => {
                    self.push(v.clone());
                }
                Opcode::Pop => {
                    self.pop()?;
                }
                Opcode::Rot => {
                    let b = self.stack.len() - 1;
                    let a = self.stack.len() - 2;
                    self.stack.swap(a, b);
                }
                Opcode::Fetch => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Object(o), String(s)) => {
                            let obj = o.borrow();
                            self.push(obj.get(s.as_ref()).cloned().unwrap_or(Null));
                        }
                        (Array(a), Number(n)) => {
                            let arr = a.borrow();
                            self.push(
                                arr.get(n.to_usize().ok_or_else(|| OpcodeErr {
                                    opcode: "Fetch".into(),
                                    message: "Failed to convert to usize".into(),
                                })?)
                                .cloned()
                                .unwrap_or(Null),
                            )
                        }
                        (String(str), Number(n)) => {
                            let index = n.to_usize().ok_or_else(|| OpcodeErr {
                                opcode: "Fetch".into(),
                                message: "Failed to convert to usize".into(),
                            })?;

                            if let Some(slice) = str.get(index..index + 1) {
                                self.push(String(Rc::from(slice)));
                            } else {
                                self.push(Null)
                            };
                        }
                        _ => self.push(Null),
                    }
                }
                Opcode::FetchEnv(f) => match &env {
                    Object(o) => {
                        let obj = o.borrow();
                        match obj.get(*f) {
                            None => self.push(Null),
                            Some(v) => self.push(v.clone()),
                        }
                    }
                    Null => self.push(Null),
                    _ => {
                        return Err(OpcodeErr {
                            opcode: "FetchEnv".into(),
                            message: "Unsupported type".into(),
                        });
                    }
                },
                Opcode::FetchRootEnv => {
                    self.push(env.clone());
                }
                Opcode::Negate => {
                    let a = self.pop()?;
                    match a {
                        Number(n) => {
                            self.push(Number(-n));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Negate".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Not => {
                    let a = self.pop()?;
                    match a {
                        Bool(b) => self.push(Bool(!b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Not".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Equal => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    match (a, b) {
                        (Number(a), Number(b)) => {
                            self.push(Bool(a == b));
                        }
                        (Bool(a), Bool(b)) => {
                            self.push(Bool(a == b));
                        }
                        (String(a), String(b)) => {
                            self.push(Bool(a == b));
                        }
                        (Null, Null) => {
                            self.push(Bool(true));
                        }
                        _ => {
                            self.push(Bool(false));
                        }
                    }
                }
                Opcode::Jump(j) => self.ip += j,
                Opcode::JumpIfTrue(j) => {
                    let a = self.stack.last().ok_or_else(|| OpcodeErr {
                        opcode: "JumpIfTrue".into(),
                        message: "Undefined object key".into(),
                    })?;
                    match a {
                        Bool(a) => {
                            if *a {
                                self.ip += j;
                            }
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "JumpIfTrue".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::JumpIfFalse(j) => {
                    let a = self.stack.last().ok_or_else(|| OpcodeErr {
                        opcode: "JumpIfFalse".into(),
                        message: "Empty array".into(),
                    })?;

                    match a {
                        Bool(a) => {
                            if !*a {
                                self.ip += j;
                            }
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "JumpIfFalse".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::JumpIfNotNull(j) => {
                    let a = self.stack.last().ok_or_else(|| OpcodeErr {
                        opcode: "JumpIfNull".into(),
                        message: "Empty array".into(),
                    })?;

                    match a {
                        Null => {}
                        _ => {
                            self.ip += j;
                        }
                    }
                }
                Opcode::JumpBackward(j) => {
                    self.ip -= j;
                }
                Opcode::In => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, &b) {
                        (Number(a), Array(b)) => {
                            let arr = b.borrow();
                            let is_in = arr.iter().any(|b| match b {
                                Number(b) => a == *b,
                                _ => false,
                            });

                            self.push(Bool(is_in));
                        }
                        (Number(v), Object(_)) => {
                            let interval =
                                IntervalObject::try_from_object(b).ok_or_else(|| OpcodeErr {
                                    opcode: "In".into(),
                                    message: "Failed to deconstruct interval".into(),
                                })?;

                            match (interval.left, interval.right) {
                                (Number(l), Number(r)) => {
                                    let mut is_open = false;

                                    let first = match interval.left_bracket.as_ref() {
                                        "[" => l <= v,
                                        "(" => l < v,
                                        "]" => {
                                            is_open = true;
                                            l >= v
                                        }
                                        ")" => {
                                            is_open = true;
                                            l > v
                                        }
                                        _ => {
                                            return Err(OpcodeErr {
                                                opcode: "In".into(),
                                                message: "Unsupported bracket".into(),
                                            });
                                        }
                                    };

                                    let second = match interval.right_bracket.as_ref() {
                                        "]" => r >= v,
                                        ")" => r > v,
                                        "[" => r <= v,
                                        "(" => r < v,
                                        _ => {
                                            return Err(OpcodeErr {
                                                opcode: "In".into(),
                                                message: "Unsupported bracket".into(),
                                            });
                                        }
                                    };

                                    let open_stmt = is_open && (first || second);
                                    let closed_stmt = !is_open && first && second;

                                    self.push(Bool(open_stmt || closed_stmt));
                                }
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "In".into(),
                                        message: "Unsupported type".into(),
                                    });
                                }
                            }
                        }
                        (String(a), Array(b)) => {
                            let arr = b.borrow();
                            let is_in = arr.iter().any(|b| match b {
                                String(b) => &a == b,
                                _ => false,
                            });

                            self.push(Bool(is_in));
                        }
                        (String(a), Object(b)) => {
                            let obj = b.borrow();
                            self.push(Bool(obj.contains_key(a.as_ref())));
                        }
                        (Bool(a), Array(b)) => {
                            let arr = b.borrow();
                            let is_in = arr.iter().any(|b| match b {
                                Bool(b) => a == *b,
                                _ => false,
                            });

                            self.push(Bool(is_in));
                        }
                        (Null, Array(b)) => {
                            let arr = b.borrow();
                            let is_in = arr.iter().any(|b| match b {
                                Null => true,
                                _ => false,
                            });

                            self.push(Bool(is_in));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "In".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Less => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Bool(a < b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Less".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::More => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Bool(a > b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "More".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::LessOrEqual => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Bool(a <= b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "LessOrEqual".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::MoreOrEqual => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Bool(a >= b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "MoreOrEqual".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Abs => {
                    let a = self.pop()?;

                    match a {
                        Number(a) => self.push(Number(a.abs())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Abs".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Round => {
                    let var = self.pop()?;

                    match var {
                        Number(a) => self.push(Number(a.round())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Round".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Ceil => {
                    let var = self.pop()?;

                    match var {
                        Number(a) => self.push(Number(a.ceil())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Ceil".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Floor => {
                    let var = self.pop()?;

                    match var {
                        Number(a) => self.push(Number(a.floor())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Floor".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Random => {
                    let var = self.pop()?;
                    match var {
                        Number(a) => {
                            let upper_range = a.round().to_i64().ok_or_else(|| OpcodeErr {
                                opcode: "Random".into(),
                                message: "Failed to determine upper range".into(),
                            })?;

                            let random_number = fastrand::i64(0..=upper_range);
                            self.push(Number(Decimal::from(random_number)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Random".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Average => {
                    let var = self.pop()?;

                    match var {
                        Array(a) => {
                            let mut sum = Decimal::ZERO;
                            let arr = a.borrow();
                            arr.iter().try_for_each(|a| match a {
                                Number(a) => {
                                    sum += a;
                                    Ok(())
                                }
                                _ => Err(OpcodeErr {
                                    opcode: "Average".into(),
                                    message: "Invalid array value".into(),
                                }),
                            })?;

                            let avg = sum / Decimal::from(arr.len());
                            self.push(Number(avg));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Average".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Median => {
                    let Array(a) = self.pop()? else {
                        return Err(OpcodeErr {
                            opcode: "Median".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let arr = a.borrow();
                    let mut num_arr = arr
                        .iter()
                        .map(|n| match n {
                            Number(num) => Ok(*num),
                            _ => Err(OpcodeErr {
                                opcode: "Median".into(),
                                message: "Unsupported type".into(),
                            }),
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    if num_arr.len() == 0 {
                        return Err(OpcodeErr {
                            opcode: "Median".into(),
                            message: "Array is empty".into(),
                        });
                    }

                    num_arr.sort();

                    let center = num_arr.len() / 2;
                    if num_arr.len() % 2 == 1 {
                        let center_num = num_arr.get(center).ok_or_else(|| OpcodeErr {
                            opcode: "Median".into(),
                            message: "Array out of bounds".into(),
                        })?;

                        self.push(Number(*center_num));
                    } else {
                        let center_left = num_arr.get(center - 1).ok_or_else(|| OpcodeErr {
                            opcode: "Median".into(),
                            message: "Array out of bounds".into(),
                        })?;

                        let center_right = num_arr.get(center).ok_or_else(|| OpcodeErr {
                            opcode: "Median".into(),
                            message: "Array out of bounds".into(),
                        })?;

                        let median = ((*center_left) + (*center_right)) / dec!(2);
                        self.push(Number(median));
                    }
                }
                Opcode::Mode => {
                    let Array(a) = self.pop()? else {
                        return Err(OpcodeErr {
                            opcode: "Mode".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let arr = a.borrow();
                    let num_arr = arr
                        .iter()
                        .map(|n| match n {
                            Number(num) => Ok(*num),
                            _ => Err(OpcodeErr {
                                opcode: "Mode".into(),
                                message: "Unsupported type".into(),
                            }),
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    if num_arr.len() == 0 {
                        return Err(OpcodeErr {
                            opcode: "Mode".into(),
                            message: "Array is empty".into(),
                        });
                    }

                    let mut map = HashMap::new();
                    num_arr.iter().for_each(|n| {
                        let count = map.entry(*n).or_insert(0);
                        *count += 1;
                    });

                    let maybe_mode = map
                        .iter()
                        .max_by_key(|&(_, count)| *count)
                        .map(|(val, _)| val);

                    let mode = maybe_mode.ok_or_else(|| OpcodeErr {
                        opcode: "Mode".into(),
                        message: "Failed to find most common element".into(),
                    })?;

                    self.push(Number(*mode));
                }
                Opcode::Min => {
                    let var = self.pop()?;

                    match var {
                        Array(a) => {
                            let arr = a.borrow();
                            let first_item = arr.get(0).ok_or_else(|| OpcodeErr {
                                opcode: "Min".into(),
                                message: "Empty array".into(),
                            })?;

                            let mut min: Decimal = match first_item {
                                Number(a) => *a,
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "Min".into(),
                                        message: "Unsupported array value".into(),
                                    });
                                }
                            };

                            arr.iter().try_for_each(|a| match a {
                                Number(a) => {
                                    if *a < min {
                                        min = *a;
                                    }
                                    Ok(())
                                }
                                _ => Err(OpcodeErr {
                                    opcode: "Min".into(),
                                    message: "Unsupported array value".into(),
                                }),
                            })?;

                            self.push(Number(min));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Min".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Max => {
                    let var = self.pop()?;

                    match var {
                        Array(a) => {
                            let arr = a.borrow();
                            let first_item = arr.get(0).ok_or_else(|| OpcodeErr {
                                opcode: "Max".into(),
                                message: "Empty array".into(),
                            })?;

                            let mut max: Decimal = match first_item {
                                Number(a) => *a,
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "Max".into(),
                                        message: "Unsupported array value".into(),
                                    });
                                }
                            };

                            arr.iter().try_for_each(|a| match a {
                                Number(a) => {
                                    if *a > max {
                                        max = *a;
                                    }
                                    Ok(())
                                }
                                _ => Err(OpcodeErr {
                                    opcode: "Max".into(),
                                    message: "Unsupported array value".into(),
                                }),
                            })?;

                            self.push(Number(max));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Max".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Sum => {
                    let var = self.pop()?;

                    match var {
                        Array(a) => {
                            let mut sum = Decimal::ZERO;
                            let arr = a.borrow();
                            arr.iter().try_for_each(|a| match a {
                                Number(a) => {
                                    sum += a;
                                    Ok(())
                                }
                                _ => Err(OpcodeErr {
                                    opcode: "Sum".into(),
                                    message: "Unsupported array value".into(),
                                }),
                            })?;

                            self.push(Number(sum));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Sum".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Add => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Number(a + b)),
                        (String(a), String(b)) => {
                            let mut c = StdString::with_capacity(a.len() + b.len());

                            c.push_str(a.as_ref());
                            c.push_str(b.as_ref());

                            self.push(String(Rc::from(c.as_str())));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Add".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Subtract => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Number(a - b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Subtract".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Multiply => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Number(a * b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Multiply".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Divide => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Number(a / b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Divide".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Modulo => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Number(a % b)),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Modulo".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Exponent => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => {
                            self.push(Number(a.powd(b)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Exponent".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Interval {
                    left_bracket,
                    right_bracket,
                } => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (&a, &b) {
                        (Number(_), Number(_)) => {
                            let interval = IntervalObject {
                                left_bracket: Rc::from(*left_bracket),
                                right_bracket: Rc::from(*right_bracket),
                                left: a,
                                right: b,
                            };

                            self.push(interval.to_variable());
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Interval".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Uppercase => {
                    let a = self.pop()?;

                    match a {
                        String(a) => {
                            self.push(String(a.to_uppercase().into()));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Uppercase".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Lowercase => {
                    let a = self.pop()?;

                    match a {
                        String(a) => self.push(String(a.to_lowercase().into())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Lowercase".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Contains => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, &b) {
                        (String(a), String(b)) => {
                            self.push(Bool(a.contains(b.as_ref())));
                        }
                        (Array(a), _) => {
                            let arr = a.borrow();
                            let is_in = arr.iter().any(|a| match (a, &b) {
                                (Number(a), Number(b)) => a == b,
                                (String(a), String(b)) => a == b,
                                (Bool(a), Bool(b)) => a == b,
                                (Null, Null) => true,
                                _ => false,
                            });

                            self.push(Bool(is_in));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Contains".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Keys => {
                    let current = self.pop()?;

                    match current {
                        Array(a) => {
                            let arr = a.borrow();
                            let indices = arr
                                .iter()
                                .enumerate()
                                .map(|(index, _)| Number(index.into()))
                                .collect();

                            self.push(Array(Rc::new(RefCell::new(indices))));
                        }
                        Object(a) => {
                            let obj = a.borrow();
                            let keys = obj
                                .iter()
                                .map(|(key, _)| String(Rc::from(key.as_str())))
                                .collect();

                            self.push(Array(Rc::new(RefCell::new(keys))));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Keys".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Values => {
                    let current = self.pop()?;

                    match current {
                        Object(a) => {
                            let obj = a.borrow();
                            let values: Vec<Variable> = obj.values().cloned().collect();

                            self.push(Array(Rc::new(RefCell::new(values))));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Values".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::StartsWith => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (String(a), String(b)) => {
                            self.push(Bool(a.starts_with(b.as_ref())));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "StartsWith".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::EndsWith => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (String(a), String(b)) => {
                            self.push(Bool(a.ends_with(b.as_ref())));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "EndsWith".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Matches => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    let (String(a), String(b)) = (a, b) else {
                        return Err(OpcodeErr {
                            opcode: "Matches".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let regex = Regex::new(b.as_ref()).map_err(|_| OpcodeErr {
                        opcode: "Matches".into(),
                        message: "Invalid regular expression".into(),
                    })?;

                    self.push(Bool(regex.is_match(a.as_ref())));
                }
                Opcode::FuzzyMatch => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    let String(b) = b else {
                        return Err(OpcodeErr {
                            opcode: "FuzzyMatch".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    match a {
                        String(a) => {
                            let sim =
                                strsim::normalized_damerau_levenshtein(a.as_ref(), b.as_ref());
                            // This is okay, as NDL will return [0, 1]
                            self.push(Number(Decimal::from_f64(sim).unwrap_or(dec!(0))));
                        }
                        Array(_a) => {
                            let a = _a.borrow();
                            let mut sims = Vec::with_capacity(a.len());
                            for v in a.iter() {
                                let String(s) = v else {
                                    return Err(OpcodeErr {
                                        opcode: "FuzzyMatch".into(),
                                        message: "Unsupported type".into(),
                                    });
                                };

                                let sim = Decimal::from_f64(
                                    strsim::normalized_damerau_levenshtein(s.as_ref(), b.as_ref()),
                                )
                                .unwrap_or(dec!(0));
                                sims.push(Number(sim));
                            }

                            self.push(Variable::from_array(sims))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "FuzzyMatch".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Split => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (String(a), String(b)) => {
                            let arr = Vec::from_iter(
                                a.split(b.as_ref())
                                    .into_iter()
                                    .map(|s| String(s.to_string().into())),
                            );

                            self.push(Variable::from_array(arr));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Split".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Join => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    let (Array(a), String(separator)) = (a, &b) else {
                        return Err(OpcodeErr {
                            opcode: "Join".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let arr = a.borrow();
                    let parts = arr
                        .iter()
                        .enumerate()
                        .map(|(i, var)| match var {
                            String(str) => Ok(str.clone()),
                            _ => Err(OpcodeErr {
                                opcode: "Join".into(),
                                message: format!("Unexpected type in array on index {i}"),
                            }),
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    let str_capacity = parts
                        .iter()
                        .fold(separator.len() * (parts.len() - 1), |acc, s| acc + s.len());

                    let mut s = StdString::with_capacity(str_capacity);
                    let mut it = parts.into_iter().peekable();
                    while let Some(part) = it.next() {
                        s.push_str(part.as_ref());
                        if it.peek().is_some() {
                            s.push_str(separator);
                        }
                    }

                    self.push(String(Rc::from(s)));
                }
                Opcode::Extract => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    let (String(a), String(b)) = (a, b) else {
                        return Err(OpcodeErr {
                            opcode: "Matches".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let regex = Regex::new(b.as_ref()).map_err(|_| OpcodeErr {
                        opcode: "Matches".into(),
                        message: "Invalid regular expression".into(),
                    })?;

                    let captures = regex
                        .captures(a.as_ref())
                        .map(|capture| {
                            capture
                                .iter()
                                .map(|c| c.map(|c| c.as_str()))
                                .filter_map(|c| c)
                                .map(|s| String(Rc::from(s)))
                                .collect()
                        })
                        .unwrap_or_default();

                    self.push(Variable::from_array(captures));
                }
                Opcode::DateManipulation(operation) => {
                    let timestamp = self.pop()?;

                    let time: NaiveDateTime = (&timestamp).try_into()?;
                    let var = match *operation {
                        "year" => Number(time.year().into()),
                        "dayOfWeek" => Number(time.weekday().number_from_monday().into()),
                        "dayOfMonth" => Number(time.day().into()),
                        "dayOfYear" => Number(time.ordinal().into()),
                        "weekOfYear" => Number(time.iso_week().week().into()),
                        "monthOfYear" => Number(time.month().into()),
                        "monthString" => String(Rc::from(time.format("%b").to_string())),
                        "weekdayString" => String(Rc::from(time.weekday().to_string())),
                        "dateString" => String(Rc::from(time.to_string())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "DateManipulation".into(),
                                message: "Unsupported operation".into(),
                            });
                        }
                    };

                    self.push(var);
                }
                Opcode::DateFunction(name) => {
                    let unit_var = self.pop()?;
                    let timestamp = self.pop()?;

                    let date_time: NaiveDateTime = (&timestamp).try_into()?;
                    let String(unit_name) = unit_var else {
                        return Err(OpcodeErr {
                            opcode: "DateFunction".into(),
                            message: "Unknown date function".into(),
                        });
                    };

                    let s = match *name {
                        "startOf" => date_time_start_of(date_time, unit_name.as_ref().try_into()?),
                        "endOf" => date_time_end_of(date_time, unit_name.as_ref().try_into()?),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "DateManipulation".into(),
                                message: "Unsupported operation".into(),
                            });
                        }
                    }
                    .ok_or_else(|| OpcodeErr {
                        opcode: "DateFunction".into(),
                        message: "Failed to run DateFunction".into(),
                    })?;

                    #[allow(deprecated)]
                    self.push(Number(s.timestamp().into()));
                }
                Opcode::Slice => {
                    let from_var = self.pop()?;
                    let to_var = self.pop()?;
                    let current = self.pop()?;

                    match (from_var, to_var) {
                        (Number(f), Number(t)) => {
                            let from = f.to_usize().ok_or_else(|| OpcodeErr {
                                opcode: "Slice".into(),
                                message: "Failed to get range from".into(),
                            })?;
                            let to = t.to_usize().ok_or_else(|| OpcodeErr {
                                opcode: "Slice".into(),
                                message: "Failed to get range to".into(),
                            })?;

                            match current {
                                Array(a) => {
                                    let arr = a.borrow();
                                    let slice = arr.get(from..=to).ok_or_else(|| OpcodeErr {
                                        opcode: "Slice".into(),
                                        message: "Index out of range".into(),
                                    })?;

                                    self.push(Variable::from_array(slice.to_vec()));
                                }
                                String(s) => {
                                    let slice = s.get(from..=to).ok_or_else(|| OpcodeErr {
                                        opcode: "Slice".into(),
                                        message: "Index out of range".into(),
                                    })?;

                                    self.push(String(Rc::from(slice)));
                                }
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "Slice".into(),
                                        message: "Unsupported type".into(),
                                    });
                                }
                            }
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Slice".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    }
                }
                Opcode::Array => {
                    let size = self.pop()?;
                    let Number(s) = size else {
                        return Err(OpcodeErr {
                            opcode: "Array".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let to = s.round().to_usize().ok_or_else(|| OpcodeErr {
                        opcode: "Array".into(),
                        message: "Failed to extract argument".into(),
                    })?;

                    let mut arr = Vec::with_capacity(to);
                    for _ in 0..to {
                        arr.push(self.pop()?);
                    }
                    arr.reverse();

                    self.push(Variable::from_array(arr));
                }
                Opcode::Object => {
                    let size = self.pop()?;
                    let Number(s) = size else {
                        return Err(OpcodeErr {
                            opcode: "Array".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let to = s.round().to_usize().ok_or_else(|| OpcodeErr {
                        opcode: "Array".into(),
                        message: "Failed to extract argument".into(),
                    })?;

                    let mut map = HashMap::with_capacity(to);
                    for _ in 0..to {
                        let value = self.pop()?;
                        let String(key) = self.pop()? else {
                            return Err(OpcodeErr {
                                opcode: "Object".into(),
                                message: "Unexpected key value".to_string(),
                            });
                        };

                        map.insert(key.to_string(), value);
                    }

                    self.push(Variable::from_object(map));
                }
                Opcode::Len => {
                    let current = self.stack.last().ok_or_else(|| OpcodeErr {
                        opcode: "Len".into(),
                        message: "Empty stack".into(),
                    })?;

                    let len = match current {
                        String(s) => s.len(),
                        Array(s) => {
                            let arr = s.borrow();
                            arr.len()
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Len".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    };

                    self.push(Number(len.into()));
                }
                Opcode::Flatten => {
                    let current = self.pop()?;
                    let Array(a) = current else {
                        return Err(OpcodeErr {
                            opcode: "Flatten".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let arr = a.borrow();

                    let mut flat_arr = Vec::with_capacity(arr.len());
                    arr.iter().for_each(|v| match v {
                        Array(b) => {
                            let arr = b.borrow();
                            arr.iter().for_each(|v| flat_arr.push(v.clone()))
                        }
                        _ => flat_arr.push(v.clone()),
                    });

                    self.push(Variable::from_array(flat_arr));
                }
                Opcode::ParseDateTime => {
                    let a = self.pop()?;
                    let ts = match a {
                        #[allow(deprecated)]
                        String(a) => date_time(a.as_ref())?.timestamp(),
                        Number(a) => a.to_i64().ok_or_else(|| OpcodeErr {
                            opcode: "ParseDateTime".into(),
                            message: "Number overflow".into(),
                        })?,
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "ParseDateTime".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    };

                    self.push(Number(ts.into()));
                }
                Opcode::ParseTime => {
                    let a = self.pop()?;
                    let ts = match a {
                        String(a) => time(a.as_ref())?.num_seconds_from_midnight(),
                        Number(a) => a.to_u32().ok_or_else(|| OpcodeErr {
                            opcode: "ParseTime".into(),
                            message: "Number overflow".into(),
                        })?,
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "ParseTime".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    };

                    self.push(Number(ts.into()));
                }
                Opcode::ParseDuration => {
                    let a = self.pop()?;

                    let dur = match a {
                        String(a) => humantime::parse_duration(a.as_ref())
                            .map_err(|_| ParseDateTimeErr {
                                timestamp: a.to_string(),
                            })?
                            .as_secs(),
                        Number(n) => n.to_u64().ok_or_else(|| OpcodeErr {
                            opcode: "ParseDuration".into(),
                            message: "Number overflow".into(),
                        })?,
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "ParseDuration".into(),
                                message: "Unsupported type".into(),
                            });
                        }
                    };

                    self.push(Number(dur.into()));
                }
                Opcode::TypeCheck(check) => {
                    let var = self.pop()?;

                    let is_equal = match (check, var) {
                        (TypeCheckKind::Numeric, String(str)) => {
                            Decimal::from_str_exact(str.as_ref()).is_ok()
                        }
                        (TypeCheckKind::Numeric, Number(_)) => true,
                        (TypeCheckKind::Numeric, _) => false,
                    };

                    self.push(Bool(is_equal));
                }
                Opcode::TypeConversion(conversion) => {
                    let var = self.pop()?;

                    let converted_var = match (conversion, &var) {
                        (TypeConversionKind::String, String(_)) => var,
                        (TypeConversionKind::String, Number(num)) => {
                            String(Rc::from(num.to_string().as_str()))
                        }
                        (TypeConversionKind::String, Bool(v)) => {
                            String(Rc::from(v.to_string().as_str()))
                        }
                        (TypeConversionKind::String, Null) => String(Rc::from("null")),
                        (TypeConversionKind::String, _) => {
                            return Err(OpcodeErr {
                                opcode: "TypeConversion".into(),
                                message: format!(
                                    "Type {} cannot be converted to string",
                                    var.type_name()
                                ),
                            });
                        }
                        (TypeConversionKind::Number, String(str)) => {
                            let parsed_number =
                                Decimal::from_str_exact(str.trim()).map_err(|_| OpcodeErr {
                                    opcode: "TypeConversion".into(),
                                    message: "Failed to parse string to number".into(),
                                })?;

                            Number(parsed_number)
                        }
                        (TypeConversionKind::Number, Number(_)) => var,
                        (TypeConversionKind::Number, Bool(v)) => {
                            let number = if *v { dec!(1) } else { dec!(0) };
                            Number(number)
                        }
                        (TypeConversionKind::Number, _) => {
                            return Err(OpcodeErr {
                                opcode: "TypeConversion".into(),
                                message: format!(
                                    "Type {} cannot be converted to number",
                                    var.type_name()
                                ),
                            });
                        }
                        (TypeConversionKind::Bool, Number(n)) => Bool(!n.is_zero()),
                        (TypeConversionKind::Bool, String(s)) => {
                            let value = match (*s).trim() {
                                "true" => true,
                                "false" => false,
                                _ => s.is_empty(),
                            };

                            Bool(value)
                        }
                        (TypeConversionKind::Bool, Bool(_)) => var,
                        (TypeConversionKind::Bool, Null) => Bool(false),
                        (TypeConversionKind::Bool, Object(_) | Array(_)) => Bool(true),
                    };

                    self.push(converted_var);
                }
                Opcode::GetType => {
                    let var = self.pop()?;
                    self.push(String(Rc::from(var.type_name())));
                }
                Opcode::JumpIfEnd(j) => {
                    let scope = self.scopes.last().ok_or_else(|| OpcodeErr {
                        opcode: "JumpIfEnd".into(),
                        message: "Empty stack".into(),
                    })?;

                    if scope.iter >= scope.len {
                        self.ip += j;
                    }
                }
                Opcode::IncrementIt => {
                    let scope = self.scopes.last_mut().ok_or_else(|| OpcodeErr {
                        opcode: "IncrementIt".into(),
                        message: "Empty scope".into(),
                    })?;

                    scope.iter += 1;
                }
                Opcode::IncrementCount => {
                    let scope = self.scopes.last_mut().ok_or_else(|| OpcodeErr {
                        opcode: "IncrementCount".into(),
                        message: "Empty scope".into(),
                    })?;

                    scope.count += 1;
                }
                Opcode::GetCount => {
                    let scope = self.scopes.last().ok_or_else(|| OpcodeErr {
                        opcode: "GetCount".into(),
                        message: "Empty scope".into(),
                    })?;

                    self.push(Number(scope.count.into()));
                }
                Opcode::GetLen => {
                    let scope = self.scopes.last().ok_or_else(|| OpcodeErr {
                        opcode: "GetLen".into(),
                        message: "Empty scope".into(),
                    })?;

                    self.push(Number(scope.len.into()));
                }
                Opcode::Pointer => {
                    let scope = self.scopes.last().ok_or_else(|| OpcodeErr {
                        opcode: "Pointer".into(),
                        message: "Empty scope".into(),
                    })?;

                    match &scope.array {
                        Array(a) => {
                            let a_cloned = a.clone();
                            let arr = a_cloned.borrow();
                            let variable =
                                arr.get(scope.iter).cloned().ok_or_else(|| OpcodeErr {
                                    opcode: "Pointer".into(),
                                    message: "Scope array out of bounds".into(),
                                })?;

                            self.push(variable);
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Pointer".into(),
                                message: "Unsupported scope type".into(),
                            });
                        }
                    }
                }
                Opcode::Begin => {
                    let a = self.pop()?;
                    let arr_len = match &a {
                        Array(a) => {
                            let arr = a.borrow();
                            Some(arr.len())
                        }
                        _ => None,
                    };

                    match arr_len {
                        Some(len) => self.scopes.push(Scope {
                            array: a,
                            count: 0,
                            len,
                            iter: 0,
                        }),
                        None => {
                            return Err(OpcodeErr {
                                opcode: "Begin".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::End => {
                    self.scopes.pop();
                }
            }
        }

        self.pop()
    }
}
