use std::collections::HashMap;

use bumpalo::Bump;
use chrono::NaiveDateTime;
use chrono::{Datelike, Timelike};
#[cfg(not(feature = "regex-lite"))]
use regex::Regex;
#[cfg(feature = "regex-lite")]
use regex_lite::Regex;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, MathematicalOps};
use rust_decimal_macros::dec;
use thiserror::Error;

use crate::helpers::{date_time, date_time_end_of, date_time_start_of, time};
use crate::opcodes::Variable::{Array, Bool, Null, Number, Object, String};
use crate::opcodes::{IntervalObject, Opcode, TypeCheckKind, TypeConversionKind, Variable};
use crate::vm::VMError::{OpcodeErr, OpcodeOutOfBounds, ParseDateTimeErr, StackOutOfBounds};

const NULL_VAR: &'static Variable = &Null;

#[derive(Debug, Error)]
pub enum VMError {
    #[error("Unsupported opcode type")]
    OpcodeErr {
        opcode: std::string::String,
        message: std::string::String,
    },

    #[error("Opcode out of bounds")]
    OpcodeOutOfBounds {
        index: usize,
        bytecode: std::string::String,
    },

    #[error("Stack out of bounds")]
    StackOutOfBounds { stack: std::string::String },

    #[error("Failed to parse date time")]
    ParseDateTimeErr { timestamp: std::string::String },
}

pub struct Scope<'a> {
    array: &'a Variable<'a>,
    len: usize,
    iter: usize,
    count: usize,
}

pub struct VM<'a> {
    scopes: &'a mut Vec<Scope<'a>>,
    stack: &'a mut Vec<&'a Variable<'a>>,
    ip: usize,
    bytecode: &'a Vec<&'a Opcode<'a>>,
    bump: &'a Bump,
}

impl<'a> VM<'a> {
    pub fn new(
        bytecode: &'a Vec<&'a Opcode<'a>>,
        stack: &'a mut Vec<&'a Variable<'a>>,
        scopes: &'a mut Vec<Scope<'a>>,
        bump: &'a Bump,
    ) -> Self {
        Self {
            scopes,
            stack,
            ip: 0,
            bytecode,
            bump,
        }
    }

    fn push(&mut self, var: Variable<'a>) {
        self.stack.push(self.bump.alloc(var));
    }

    fn pop(&mut self) -> Result<&'a Variable<'a>, VMError> {
        self.stack.pop().ok_or_else(|| StackOutOfBounds {
            stack: format!("{:?}", self.stack),
        })
    }

    fn push_ref(&mut self, var: &'a Variable<'a>) {
        self.stack.push(var);
    }

    pub fn run(&mut self, env: &'a Variable<'a>) -> Result<&'a Variable<'a>, VMError> {
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
                    self.push_ref(v);
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
                            self.push_ref(o.get(*s).unwrap_or(&NULL_VAR));
                        }
                        (Array(arr), Number(n)) => self.push_ref(
                            arr.get(n.to_usize().ok_or_else(|| OpcodeErr {
                                opcode: "Fetch".into(),
                                message: "Failed to convert to usize".into(),
                            })?)
                            .unwrap_or(&NULL_VAR),
                        ),
                        (String(str), Number(n)) => {
                            let index = n.to_usize().ok_or_else(|| OpcodeErr {
                                opcode: "Fetch".into(),
                                message: "Failed to convert to usize".into(),
                            })?;

                            if let Some(slice) = str.get(index..index + 1) {
                                self.push(String(self.bump.alloc_str(slice)));
                            } else {
                                self.push_ref(&NULL_VAR)
                            };
                        }
                        _ => self.push_ref(&NULL_VAR),
                    }
                }
                Opcode::FetchEnv(f) => match env {
                    Object(o) => self.push_ref(o.get(*f).unwrap_or(&NULL_VAR)),
                    Null => self.push_ref(NULL_VAR),
                    _ => {
                        return Err(OpcodeErr {
                            opcode: "FetchEnv".into(),
                            message: "Unsupported type".into(),
                        })
                    }
                },
                Opcode::Negate => {
                    let a = self.pop()?;
                    match a {
                        Number(n) => {
                            self.stack.push(self.bump.alloc(Number(-*n)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Negate".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Not => {
                    let a = self.pop()?;
                    match a {
                        Bool(b) => self.stack.push(self.bump.alloc(Bool(!(*b)))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Not".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Equal => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    match (a, b) {
                        (Number(a), Number(b)) => {
                            self.stack.push(self.bump.alloc(Bool(a == b)));
                        }
                        (Bool(a), Bool(b)) => {
                            self.stack.push(self.bump.alloc(Bool(a == b)));
                        }
                        (String(a), String(b)) => {
                            self.stack.push(self.bump.alloc(Bool(a == b)));
                        }
                        (Null, Null) => {
                            self.stack.push(self.bump.alloc(Bool(true)));
                        }
                        _ => {
                            self.stack.push(self.bump.alloc(Bool(false)));
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
                            })
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
                            })
                        }
                    }
                }
                Opcode::JumpBackward(j) => {
                    self.ip -= j;
                }
                Opcode::In => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Array(arr)) => {
                            let is_in = arr.iter().any(|b| match b {
                                Number(b) => a == b,
                                _ => false,
                            });

                            self.stack.push(self.bump.alloc(Bool(is_in)));
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

                                    let first = match interval.left_bracket {
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
                                            })
                                        }
                                    };

                                    let second = match interval.right_bracket {
                                        "]" => r >= v,
                                        ")" => r > v,
                                        "[" => r <= v,
                                        "(" => r < v,
                                        _ => {
                                            return Err(OpcodeErr {
                                                opcode: "In".into(),
                                                message: "Unsupported bracket".into(),
                                            })
                                        }
                                    };

                                    let open_stmt = is_open && (first || second);
                                    let closed_stmt = !is_open && first && second;

                                    self.stack
                                        .push(self.bump.alloc(Bool(open_stmt || closed_stmt)));
                                }
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "In".into(),
                                        message: "Unsupported type".into(),
                                    })
                                }
                            }
                        }
                        (String(a), Array(arr)) => {
                            let is_in = arr.iter().any(|b| match b {
                                String(b) => a == b,
                                _ => false,
                            });

                            self.stack.push(self.bump.alloc(Bool(is_in)));
                        }
                        (Bool(a), Array(arr)) => {
                            let is_in = arr.iter().any(|b| match b {
                                Bool(b) => a == b,
                                _ => false,
                            });

                            self.stack.push(self.bump.alloc(Bool(is_in)));
                        }
                        (Null, Array(arr)) => {
                            let is_in = arr.iter().any(|b| match b {
                                Null => true,
                                _ => false,
                            });

                            self.stack.push(self.bump.alloc(Bool(is_in)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "In".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Less => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Bool(*a < *b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Less".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::More => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Bool(*a > *b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "More".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::LessOrEqual => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Bool(*a <= *b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "LessOrEqual".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::MoreOrEqual => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Bool(*a >= *b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "MoreOrEqual".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Abs => {
                    let a = self.pop()?;

                    match a {
                        Number(a) => self.stack.push(self.bump.alloc(Number(a.abs()))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Abs".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Round => {
                    let var = self.pop()?;

                    match var {
                        Number(a) => self.stack.push(self.bump.alloc(Number(a.round()))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Round".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Ceil => {
                    let var = self.pop()?;

                    match var {
                        Number(a) => self.stack.push(self.bump.alloc(Number(a.ceil()))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Ceil".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Floor => {
                    let var = self.pop()?;

                    match var {
                        Number(a) => self.stack.push(self.bump.alloc(Number(a.floor()))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Floor".into(),
                                message: "Unsupported type".into(),
                            })
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
                            self.stack
                                .push(self.bump.alloc(Number(Decimal::from(random_number))));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Random".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Average => {
                    let var = self.pop()?;

                    match var {
                        Array(arr) => {
                            let mut sum = Decimal::ZERO;
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
                            self.stack.push(self.bump.alloc(Number(avg)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Average".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Median => {
                    let Array(arr) = self.pop()? else {
                        return Err(OpcodeErr {
                            opcode: "Median".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let mut num_arr = arr
                        .iter()
                        .map(|n| match n {
                            Number(num) => Ok(num),
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

                        self.push(Number((*center_num).clone()));
                    } else {
                        let center_left = num_arr.get(center - 1).ok_or_else(|| OpcodeErr {
                            opcode: "Median".into(),
                            message: "Array out of bounds".into(),
                        })?;

                        let center_right = num_arr.get(center).ok_or_else(|| OpcodeErr {
                            opcode: "Median".into(),
                            message: "Array out of bounds".into(),
                        })?;

                        let median = ((**center_left) + (**center_right)) / dec!(2);
                        self.push(Number(median));
                    }
                }
                Opcode::Mode => {
                    let Array(arr) = self.pop()? else {
                        return Err(OpcodeErr {
                            opcode: "Mode".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let num_arr = arr
                        .iter()
                        .map(|n| match n {
                            Number(num) => Ok(num),
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
                        let count = map.entry(**n).or_insert(0);
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
                        Array(arr) => {
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
                                    })
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

                            self.stack.push(self.bump.alloc(Number(min)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Min".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Max => {
                    let var = self.pop()?;

                    match var {
                        Array(arr) => {
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
                                    })
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

                            self.stack.push(self.bump.alloc(Number(max)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Max".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Sum => {
                    let var = self.pop()?;

                    match var {
                        Array(arr) => {
                            let mut sum = Decimal::ZERO;
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

                            self.stack.push(self.bump.alloc(Number(sum)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Sum".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Add => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Number(a + b))),
                        (String(a), String(b)) => {
                            let mut str1 = std::string::String::with_capacity(a.len() + b.len());
                            str1.push_str(a);
                            str1.push_str(b);

                            self.stack
                                .push(self.bump.alloc(String(self.bump.alloc_str(&str1))))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Add".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Subtract => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Number(a - b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Subtract".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Multiply => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Number(a * b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Multiply".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Divide => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Number(a / b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Divide".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Modulo => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => self.stack.push(self.bump.alloc(Number(a % b))),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Modulo".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Exponent => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(a), Number(b)) => {
                            self.stack.push(self.bump.alloc(Number(a.powd(*b))))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Exponent".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Interval {
                    left_bracket,
                    right_bracket,
                } => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (Number(_), Number(_)) => {
                            let interval = IntervalObject {
                                left_bracket,
                                right_bracket,
                                left: a,
                                right: b,
                            };

                            self.push(interval.cast_to_object(self.bump));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Interval".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Uppercase => {
                    let a = self.pop()?;

                    match a {
                        String(a) => {
                            let str = a.to_uppercase();
                            self.stack
                                .push(self.bump.alloc(String(self.bump.alloc_str(&str))))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Uppercase".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Lowercase => {
                    let a = self.pop()?;

                    match a {
                        String(a) => {
                            let str = a.to_lowercase();
                            self.stack
                                .push(self.bump.alloc(String(self.bump.alloc_str(&str))))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Lowercase".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Contains => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (String(a), String(b)) => {
                            self.stack.push(self.bump.alloc(Bool(a.contains(b))))
                        }
                        _ => match a {
                            Array(arr) => {
                                let is_in = arr.iter().any(|a| match (a, b) {
                                    (Number(a), Number(b)) => a == b,
                                    (String(a), String(b)) => a == b,
                                    (Bool(a), Bool(b)) => a == b,
                                    (Null, Null) => true,
                                    _ => false,
                                });

                                self.stack.push(self.bump.alloc(Bool(is_in)));
                            }
                            _ => {
                                return Err(OpcodeErr {
                                    opcode: "Contains".into(),
                                    message: "Unsupported type".into(),
                                })
                            }
                        },
                    }
                }
                Opcode::StartsWith => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (String(a), String(b)) => {
                            self.stack.push(self.bump.alloc(Bool(a.starts_with(b))))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "StartsWith".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::EndsWith => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    match (a, b) {
                        (String(a), String(b)) => {
                            self.stack.push(self.bump.alloc(Bool(a.ends_with(b))))
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "EndsWith".into(),
                                message: "Unsupported type".into(),
                            })
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

                    let regex = Regex::new(b).map_err(|_| OpcodeErr {
                        opcode: "Matches".into(),
                        message: "Invalid regular expression".into(),
                    })?;

                    self.push(Bool(regex.is_match(a)));
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

                    let regex = Regex::new(b).map_err(|_| OpcodeErr {
                        opcode: "Matches".into(),
                        message: "Invalid regular expression".into(),
                    })?;

                    let captures = regex
                        .captures(a)
                        .map(|capture| {
                            capture
                                .iter()
                                .map(|c| c.map(|c| c.as_str()))
                                .filter_map(|c| c)
                                .map(|s| {
                                    self.bump.alloc(String(self.bump.alloc_str(s))) as &Variable
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();

                    self.push(Array(self.bump.alloc_slice_copy(captures.as_slice())));
                }
                Opcode::DateManipulation(operation) => {
                    let timestamp = self.pop()?;

                    let time: NaiveDateTime = timestamp.try_into()?;
                    let var = match *operation {
                        "year" => Number(time.year().into()),
                        "dayOfWeek" => Number(time.weekday().number_from_monday().into()),
                        "dayOfMonth" => Number(time.day().into()),
                        "dayOfYear" => Number(time.ordinal().into()),
                        "weekOfYear" => Number(time.iso_week().week().into()),
                        "monthOfYear" => Number(time.month().into()),
                        "monthString" => {
                            String(self.bump.alloc_str(&time.format("%b").to_string()))
                        }
                        "weekdayString" => String(self.bump.alloc_str(&time.weekday().to_string())),
                        "dateString" => String(self.bump.alloc_str(&time.to_string())),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "DateManipulation".into(),
                                message: "Unsupported operation".into(),
                            })
                        }
                    };

                    self.stack.push(self.bump.alloc(var));
                }
                Opcode::DateFunction(name) => {
                    let unit_var = self.pop()?;
                    let timestamp = self.pop()?;

                    let date_time: NaiveDateTime = timestamp.try_into()?;
                    let String(unit_name) = *unit_var else {
                        return Err(OpcodeErr {
                            opcode: "DateFunction".into(),
                            message: "Unknown date function".into(),
                        });
                    };

                    let s = match *name {
                        "startOf" => date_time_start_of(date_time, unit_name.try_into()?),
                        "endOf" => date_time_end_of(date_time, unit_name.try_into()?),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "DateManipulation".into(),
                                message: "Unsupported operation".into(),
                            })
                        }
                    }
                    .ok_or_else(|| OpcodeErr {
                        opcode: "DateFunction".into(),
                        message: "Failed to run DateFunction".into(),
                    })?;

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
                                Array(arr) => {
                                    let slice = arr.get(from..=to).ok_or_else(|| OpcodeErr {
                                        opcode: "Slice".into(),
                                        message: "Index out of range".into(),
                                    })?;

                                    self.stack.push(self.bump.alloc(Array(slice)))
                                }
                                String(s) => {
                                    let slice = s.get(from..=to).ok_or_else(|| OpcodeErr {
                                        opcode: "Slice".into(),
                                        message: "Index out of range".into(),
                                    })?;

                                    self.stack.push(self.bump.alloc(String(slice)))
                                }
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "Slice".into(),
                                        message: "Unsupported type".into(),
                                    })
                                }
                            }
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Slice".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }
                }
                Opcode::Array => {
                    let size = self.pop()?;
                    let mut arr = Vec::new();

                    match size {
                        Number(s) => {
                            let to = s.round().to_usize().ok_or_else(|| OpcodeErr {
                                opcode: "Array".into(),
                                message: "Failed to extract argument".into(),
                            })?;

                            for _ in 0..to {
                                arr.push(self.pop()?);
                            }
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Array".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    }

                    arr.reverse();

                    self.stack.push(
                        self.bump
                            .alloc(Array(self.bump.alloc_slice_copy(arr.as_slice()))),
                    );
                }
                Opcode::Len => {
                    let current = self.stack.last().ok_or_else(|| OpcodeErr {
                        opcode: "Len".into(),
                        message: "Empty stack".into(),
                    })?;

                    let len = match current {
                        String(s) => s.len(),
                        Array(s) => s.len(),
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Len".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    };

                    self.stack.push(self.bump.alloc(Number(len.into())))
                }
                Opcode::Flatten => {
                    let current = self.pop()?;
                    let Array(arr) = current else {
                        return Err(OpcodeErr {
                            opcode: "Flatten".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    let mut flat_arr = Vec::new();
                    arr.iter().for_each(|&v| match v {
                        Array(arr) => arr.iter().for_each(|&v| flat_arr.push(v)),
                        _ => flat_arr.push(v),
                    });

                    self.stack.push(
                        self.bump
                            .alloc(Array(self.bump.alloc_slice_copy(flat_arr.as_slice()))),
                    )
                }
                Opcode::ParseDateTime => {
                    let a = self.pop()?;
                    let ts = match a {
                        String(a) => date_time(a)?.timestamp(),
                        Number(a) => a.to_i64().ok_or_else(|| OpcodeErr {
                            opcode: "ParseDateTime".into(),
                            message: "Number overflow".into(),
                        })?,
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "ParseDateTime".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    };

                    self.stack.push(self.bump.alloc(Number(ts.into())))
                }
                Opcode::ParseTime => {
                    let a = self.pop()?;
                    let ts = match a {
                        String(a) => time(a)?.num_seconds_from_midnight(),
                        Number(a) => a.to_u32().ok_or_else(|| OpcodeErr {
                            opcode: "ParseTime".into(),
                            message: "Number overflow".into(),
                        })?,
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "ParseTime".into(),
                                message: "Unsupported type".into(),
                            })
                        }
                    };
                    self.stack.push(self.bump.alloc(Number(ts.into())))
                }
                Opcode::ParseDuration => {
                    let a = self.pop()?;

                    let dur = match a {
                        String(a) => humantime::parse_duration(a)
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
                            })
                        }
                    };

                    self.stack.push(self.bump.alloc(Number(dur.into())));
                }
                Opcode::TypeCheck(check) => {
                    let var = self.pop()?;

                    let is_equal = match (check, var) {
                        (TypeCheckKind::Numeric, String(str)) => {
                            Decimal::from_str_exact(str).is_ok()
                        }
                        (TypeCheckKind::Numeric, Number(_)) => true,
                        (TypeCheckKind::Numeric, _) => false,
                    };

                    self.push(Bool(is_equal));
                }
                Opcode::TypeConversion(conversion) => {
                    let var = self.pop()?;

                    let make_string = |val: &str| self.bump.alloc(String(self.bump.alloc_str(val)));

                    let converted_var = match (conversion, var) {
                        (TypeConversionKind::String, String(_)) => var,
                        (TypeConversionKind::String, Number(num)) => {
                            make_string(num.to_string().as_str())
                        }
                        (TypeConversionKind::String, Bool(v)) => {
                            make_string(v.to_string().as_str())
                        }
                        (TypeConversionKind::String, _) => {
                            return Err(OpcodeErr {
                                opcode: "TypeConversion".into(),
                                message: format!(
                                    "Type {} cannot be converted to string",
                                    var.type_name()
                                ),
                            })
                        }
                        (TypeConversionKind::Number, String(str)) => {
                            let parsed_number =
                                Decimal::from_str_exact(str).map_err(|_| OpcodeErr {
                                    opcode: "TypeConversion".into(),
                                    message: "Failed to parse string to number".into(),
                                })?;

                            self.bump.alloc(Number(parsed_number))
                        }
                        (TypeConversionKind::Number, Number(_)) => var,
                        (TypeConversionKind::Number, Bool(v)) => {
                            let number = if *v { dec!(1) } else { dec!(0) };
                            self.bump.alloc(Number(number))
                        }
                        (TypeConversionKind::Number, _) => {
                            return Err(OpcodeErr {
                                opcode: "TypeConversion".into(),
                                message: format!(
                                    "Type {} cannot be converted to number",
                                    var.type_name()
                                ),
                            })
                        }
                    };

                    self.push_ref(converted_var);
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

                    match scope.array {
                        Array(arr) => {
                            let variable = *arr.get(scope.iter).ok_or_else(|| OpcodeErr {
                                opcode: "Pointer".into(),
                                message: "Scope array out of bounds".into(),
                            })?;

                            self.push_ref(variable);
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Pointer".into(),
                                message: "Unsupported scope type".into(),
                            })
                        }
                    }
                }
                Opcode::Begin => {
                    let a = self.pop()?;
                    match a {
                        Array(arr) => self.scopes.push(Scope {
                            array: a,
                            count: 0,
                            len: arr.len(),
                            iter: 0,
                        }),
                        _ => {
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
