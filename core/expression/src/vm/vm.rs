use crate::compiler::{Compare, FetchFastTarget, Jump, Opcode};
use crate::functions::arguments::Arguments;
use crate::functions::registry::FunctionRegistry;
use crate::functions::{internal, MethodRegistry};
use crate::variable::Variable;
use crate::variable::Variable::*;
use crate::vm::error::VMError::*;
use crate::vm::error::VMResult;
use crate::vm::interval::{VmInterval, VmIntervalData};
use ahash::{HashMap, HashMapExt};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, MathematicalOps};
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
                        (Dynamic(a), Dynamic(b)) => {
                            let a = a.as_date();
                            let b = b.as_date();

                            self.push(Bool(a.is_some() && b.is_some() && a == b));
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
                        (Number(v), Dynamic(d)) => {
                            let Some(i) = d.as_any().downcast_ref::<VmInterval>() else {
                                return Err(OpcodeErr {
                                    opcode: "In".into(),
                                    message: "Unsupported type".into(),
                                });
                            };

                            self.push(Bool(i.includes(VmIntervalData::Number(v)).map_err(
                                |err| OpcodeErr {
                                    opcode: "In".into(),
                                    message: err.to_string(),
                                },
                            )?));
                        }
                        (Dynamic(d), Dynamic(i)) => {
                            let Some(d) = d.as_date() else {
                                return Err(OpcodeErr {
                                    opcode: "In".into(),
                                    message: "Unsupported type".into(),
                                });
                            };

                            let Some(i) = i.as_any().downcast_ref::<VmInterval>() else {
                                return Err(OpcodeErr {
                                    opcode: "In".into(),
                                    message: "Unsupported type".into(),
                                });
                            };

                            self.push(Bool(i.includes(VmIntervalData::Date(d.clone())).map_err(
                                |err| OpcodeErr {
                                    opcode: "In".into(),
                                    message: err.to_string(),
                                },
                            )?));
                        }
                        (Dynamic(a), Array(arr)) => {
                            let Some(a) = a.as_date() else {
                                return Err(OpcodeErr {
                                    opcode: "In".into(),
                                    message: "Unsupported type".into(),
                                });
                            };

                            let arr = arr.borrow();
                            let is_in = arr.iter().any(|b| match b {
                                Dynamic(b) => Some(a) == b.as_date(),
                                _ => false,
                            });

                            self.push(Bool(is_in));
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
                Opcode::Compare(comparison) => {
                    let b = self.pop()?;
                    let a = self.pop()?;

                    fn compare<T: Ord>(a: &T, b: &T, comparison: &Compare) -> bool {
                        match comparison {
                            Compare::More => a > b,
                            Compare::MoreOrEqual => a >= b,
                            Compare::Less => a < b,
                            Compare::LessOrEqual => a <= b,
                        }
                    }

                    match (a, b) {
                        (Number(a), Number(b)) => self.push(Bool(compare(&a, &b, comparison))),
                        (Dynamic(a), Dynamic(b)) => {
                            let (a, b) = match (a.as_date(), b.as_date()) {
                                (Some(a), Some(b)) => (a, b),
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "Compare".into(),
                                        message: "Unsupported type".into(),
                                    })
                                }
                            };

                            self.push(Bool(compare(a, b, comparison)));
                        }
                        _ => {
                            return Err(OpcodeErr {
                                opcode: "Compare".into(),
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
                            let result = a
                                .checked_powd(b)
                                .or_else(|| Decimal::from_f64(a.to_f64()?.powf(b.to_f64()?)))
                                .ok_or_else(|| OpcodeErr {
                                    opcode: "Exponent".into(),
                                    message: "Failed to calculate exponent".into(),
                                })?;

                            self.push(Number(result));
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
                        (Number(a), Number(b)) => {
                            let interval = VmInterval {
                                left_bracket: *left_bracket,
                                right_bracket: *right_bracket,
                                left: VmIntervalData::Number(*a),
                                right: VmIntervalData::Number(*b),
                            };

                            self.push(Dynamic(Rc::new(interval)));
                        }
                        (Dynamic(a), Dynamic(b)) => {
                            let (a, b) = match (a.as_date(), b.as_date()) {
                                (Some(a), Some(b)) => (a, b),
                                _ => {
                                    return Err(OpcodeErr {
                                        opcode: "Interval".into(),
                                        message: "Unsupported type".into(),
                                    })
                                }
                            };

                            let interval = VmInterval {
                                left_bracket: *left_bracket,
                                right_bracket: *right_bracket,
                                left: VmIntervalData::Date(a.clone()),
                                right: VmIntervalData::Date(b.clone()),
                            };

                            self.push(Dynamic(Rc::new(interval)));
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

                        map.insert(key.clone(), value);
                    }

                    self.push(Variable::from_object(map));
                }
                Opcode::Len => {
                    let current = self.stack.last().ok_or_else(|| OpcodeErr {
                        opcode: "Len".into(),
                        message: "Empty stack".into(),
                    })?;

                    let len_var =
                        internal::imp::len(Arguments(&[current.clone()])).map_err(|err| {
                            OpcodeErr {
                                opcode: "Len".into(),
                                message: err.to_string(),
                            }
                        })?;

                    self.push(len_var);
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
                        _ => match var.dynamic::<VmInterval>().map(|s| s.to_array()).flatten() {
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
                Opcode::CallMethod { kind, arg_count } => {
                    let method = MethodRegistry::get_definition(kind).ok_or_else(|| OpcodeErr {
                        opcode: "CallMethod".into(),
                        message: format!("Method `{kind}` not found"),
                    })?;

                    let params_start = self.stack.len().saturating_sub(*arg_count as usize) - 1;
                    let result = method
                        .call(Arguments(&self.stack[params_start..]))
                        .map_err(|err| OpcodeErr {
                            opcode: "CallMethod".into(),
                            message: format!("Method `{kind}` failed: {err}"),
                        })?;

                    self.stack.drain(params_start..);
                    self.push(result);
                }
            }
        }

        self.pop()
    }
}
