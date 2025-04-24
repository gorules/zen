use crate::compiler::{FetchFastTarget, Jump, Opcode, TypeConversionKind};
use crate::functions::registry::FunctionRegistry;
use crate::functions::Arguments;
use crate::lexer::Bracket;
use crate::variable::Variable;
use crate::variable::Variable::*;
use crate::vm::error::VMError::*;
use crate::vm::error::VMResult;
use crate::vm::helpers::{date_time, date_time_end_of, date_time_start_of, time};
use crate::vm::variable::IntervalObject;
use ahash::{HashMap, HashMapExt};
use chrono::NaiveDateTime;
use chrono::{Datelike, Timelike};
#[cfg(feature = "regex-lite")]
use regex_lite::Regex;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, MathematicalOps};
use rust_decimal_macros::dec;
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

struct VMInner<'parent_ref, 'bytecode_ref> {
    scopes: &'parent_ref mut Vec<Scope>,
    stack: &'parent_ref mut Vec<Variable>,
    bytecode: &'bytecode_ref [Opcode],
    ip: u32,
}

impl<'arena, 'parent_ref, 'bytecode_ref> VMInner<'parent_ref, 'bytecode_ref> {
    pub fn new(
        bytecode: &'bytecode_ref [Opcode],
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

        while self.ip < self.bytecode.len() as u32 {
            let op = self
                .bytecode
                .get(self.ip as usize)
                .ok_or_else(|| OpcodeOutOfBounds {
                    bytecode: format!("{:?}", self.bytecode),
                    index: self.ip as usize,
                })?;

            self.ip += 1;

            match op {
                Opcode::PushNull => self.push(Null),
                Opcode::PushBool(b) => self.push(Bool(*b)),
                Opcode::PushNumber(n) => self.push(Number(*n)),
                Opcode::PushString(s) => self.push(String(Rc::from(s.as_ref()))),
                Opcode::Pop => {
                    self.pop()?;
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
                Opcode::FetchFast(path) => {
                    let variable = path.iter().fold(Null, |v, p| match p {
                        FetchFastTarget::Root => env.clone(),
                        FetchFastTarget::String(key) => match v {
                            Object(obj) => {
                                let obj_ref = obj.borrow();
                                obj_ref.get(key.as_ref()).cloned().unwrap_or(Null)
                            }
                            _ => Null,
                        },
                        FetchFastTarget::Number(num) => match v {
                            Array(arr) => {
                                let arr_ref = arr.borrow();
                                arr_ref.get(*num as usize).cloned().unwrap_or(Null)
                            }
                            _ => Null,
                        },
                    });

                    self.push(variable);
                }
                Opcode::FetchEnv(f) => match &env {
                    Object(o) => {
                        let obj = o.borrow();
                        match obj.get(f.as_ref()) {
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
                Opcode::Jump(kind, j) => match kind {
                    Jump::Forward => self.ip += j,
                    Jump::Backward => self.ip -= j,
                    Jump::IfTrue => {
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
                    Jump::IfFalse => {
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
                    Jump::IfNotNull => {
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
                    Jump::IfEnd => {
                        let scope = self.scopes.last().ok_or_else(|| OpcodeErr {
                            opcode: "JumpIfEnd".into(),
                            message: "Empty stack".into(),
                        })?;

                        if scope.iter >= scope.len {
                            self.ip += j;
                        }
                    }
                },
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

                                    let first = match interval.left_bracket {
                                        Bracket::LeftParenthesis => l < v,
                                        Bracket::LeftSquareBracket => l <= v,
                                        Bracket::RightParenthesis => {
                                            is_open = true;
                                            l > v
                                        }
                                        Bracket::RightSquareBracket => {
                                            is_open = true;
                                            l >= v
                                        }
                                        _ => {
                                            return Err(OpcodeErr {
                                                opcode: "In".into(),
                                                message: "Unsupported bracket".into(),
                                            })
                                        }
                                    };

                                    let second = match interval.right_bracket {
                                        Bracket::RightParenthesis => r > v,
                                        Bracket::RightSquareBracket => r >= v,
                                        Bracket::LeftParenthesis => r < v,
                                        Bracket::LeftSquareBracket => r <= v,
                                        _ => {
                                            return Err(OpcodeErr {
                                                opcode: "In".into(),
                                                message: "Unsupported bracket".into(),
                                            })
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
                                left_bracket: *left_bracket,
                                right_bracket: *right_bracket,
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
                Opcode::DateManipulation(operation) => {
                    let timestamp = self.pop()?;

                    let time: NaiveDateTime = (&timestamp).try_into()?;
                    let var = match operation.as_ref() {
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

                    let s = match name.as_ref() {
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
                    let var = self.pop()?;
                    let maybe_scope = match &var {
                        Array(a) => {
                            let arr = a.borrow();
                            Some(Scope {
                                len: arr.len(),
                                array: var.clone(),
                                count: 0,
                                iter: 0,
                            })
                        }
                        _ => match IntervalObject::try_from_object(var)
                            .map(|s| s.to_array())
                            .flatten()
                        {
                            None => None,
                            Some(arr) => Some(Scope {
                                len: arr.len(),
                                array: Variable::from_array(arr),
                                count: 0,
                                iter: 0,
                            }),
                        },
                    };

                    let Some(scope) = maybe_scope else {
                        return Err(OpcodeErr {
                            opcode: "Begin".into(),
                            message: "Unsupported type".into(),
                        });
                    };

                    self.scopes.push(scope);
                }
                Opcode::End => {
                    self.scopes.pop();
                }
                Opcode::CallFunction { kind, arg_count } => {
                    let function =
                        FunctionRegistry::get_definition(kind).ok_or_else(|| OpcodeErr {
                            opcode: "CallFunction".into(),
                            message: format!("Function `{kind}` not found"),
                        })?;

                    let params_start = self.stack.len().saturating_sub(*arg_count as usize);
                    let result = function
                        .call(Arguments(&self.stack[params_start..]))
                        .map_err(|err| OpcodeErr {
                            opcode: "CallFunction".into(),
                            message: format!("Function `{kind}` failed: {err}"),
                        })?;

                    self.stack.drain(params_start..);
                    self.push(result);
                }
            }
        }

        self.pop()
    }
}
