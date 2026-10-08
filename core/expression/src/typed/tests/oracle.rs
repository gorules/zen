use crate::compiler::Opcode;
use crate::typed::tests::workload::{Block, Dialect, Workload};
use crate::typed::{Hir, Scope};
use crate::variable::Variable;
use crate::Isolate;
use rust_decimal::Decimal;
use std::fmt;
use std::sync::Arc;

type Code = Arc<[Opcode]>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    Null,
    Num(Decimal),
    Bool(bool),
    Str(String),
    List(Vec<Val>),
    Other(String),
}

pub(crate) type RowResult = Result<Vec<Val>, String>;

struct TableRule {
    cells: Vec<Option<Code>>,
    values: Vec<Option<Code>>,
}

struct Target {
    name: String,
    member: Option<(String, String)>,
}

enum Step {
    Expr { key: String, code: Code },
    Table { keys: Vec<String>, inputs: Vec<Code>, rules: Vec<TableRule> },
}

pub(crate) struct Oracle {
    dialect: Dialect,
    each: Option<(String, String)>,
    steps: Vec<Step>,
    outputs: Vec<Target>,
}

impl Val {
    pub(crate) fn of(value: &Variable) -> Val {
        match value {
            Variable::Null => Val::Null,
            Variable::Number(n) => Val::Num(*n),
            Variable::Bool(b) => Val::Bool(*b),
            Variable::String(s) => Val::Str(s.to_string()),
            Variable::Array(items) => Val::List(items.borrow().iter().map(Val::of).collect()),
            other => Val::Other(format!("{}", other.to_value())),
        }
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Val::Null => write!(f, "null"),
            Val::Num(n) => write!(f, "{n}"),
            Val::Bool(b) => write!(f, "{b}"),
            Val::Str(s) => write!(f, "{s:?}"),
            Val::List(items) => write!(f, "[{}]", items.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")),
            Val::Other(s) => write!(f, "<{s}>"),
        }
    }
}

impl Oracle {
    pub(crate) fn new(workload: &Workload, hir: &Hir) -> Self {
        let mut isolate = Isolate::new();
        let mut standard = |source: &str| -> Code { isolate.compile_standard(source).map(|e| e.bytecode().clone()).expect("standard source") };
        let mut steps = Vec::with_capacity(workload.blocks.len());
        for block in &workload.blocks {
            steps.push(match block {
                Block::Expr { key, source } => Step::Expr { key: key.clone(), code: standard(source) },
                Block::Table { inputs, outputs, rules } => {
                    let inputs = inputs.iter().map(|s| standard(s)).collect();
                    let mut compiled = Vec::with_capacity(rules.len());
                    for rule in rules {
                        let cells = rule
                            .cells
                            .iter()
                            .map(|cell| match cell.trim().is_empty() {
                                true => None,
                                false => Some(Isolate::new().compile_unary(cell).map(|e| e.bytecode().clone()).expect("unary cell")),
                            })
                            .collect();
                        let values = (0..outputs.len())
                            .map(|j| {
                                let source = rule.values.get(j).map(String::as_str).unwrap_or("");
                                (!source.trim().is_empty()).then(|| standard(source))
                            })
                            .collect();
                        compiled.push(TableRule { cells, values });
                    }
                    Step::Table { keys: outputs.clone(), inputs, rules: compiled }
                }
            });
        }
        let each = workload.member.and_then(|m| workload.spec.lists.get(m)).map(|l| (l.path.to_string(), l.alias.to_string()));
        let mut outputs = Vec::with_capacity(hir.outputs.len());
        for o in &hir.outputs {
            let member = match (o.scope, &each) {
                (Scope::Elem(_), Some((path, alias))) => {
                    let field = o.name.strip_prefix(&format!("{alias}.")).expect("member output").to_string();
                    Some((path.clone(), field))
                }
                _ => None,
            };
            outputs.push(Target { name: o.name.clone(), member });
        }
        Self { dialect: workload.dialect, each, steps, outputs }
    }

    fn hit<'r>(&self, isolate: &mut Isolate, inputs: &[Code], rules: &'r [TableRule]) -> Result<Option<&'r TableRule>, String> {
        for rule in rules {
            let mut matched = true;
            for (cell, input) in rule.cells.iter().zip(inputs) {
                let Some(cell) = cell else {
                    continue;
                };
                let outcome = isolate.run_compiled(input).map_err(|e| e.to_string()).and_then(|value| {
                    let _ = isolate.set_reference_value(value);
                    isolate.run_unary_compiled(cell).map_err(|e| e.to_string())
                });
                let pass = match (outcome, self.dialect) {
                    (Ok(pass), _) => pass,
                    (Err(_), Dialect::Graph) => false,
                    (Err(e), Dialect::Policy) => return Err(e),
                };
                if !pass {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Ok(Some(rule));
            }
        }
        Ok(None)
    }

    pub(crate) fn run(&self, input: Variable) -> RowResult {
        let mut isolate = Isolate::new();
        let result = Variable::empty_object();
        isolate.set_environment(input.shallow_clone());
        for step in &self.steps {
            match (step, &self.each) {
                (Step::Expr { key, code }, None) => {
                    let value = isolate.run_compiled(code).map_err(|e| e.to_string())?;
                    self.write(&mut isolate, &input, &result, key, value);
                }
                (Step::Expr { key, code }, Some((path, alias))) => {
                    let items = match input.dot(path) {
                        Some(Variable::Array(items)) => items.borrow().clone(),
                        _ => Vec::new(),
                    };
                    for item in items {
                        let env = Variable::empty_object();
                        if let Variable::Object(root) = &input {
                            for (k, v) in root.borrow().iter() {
                                env.dot_insert(k, v.shallow_clone());
                            }
                        }
                        env.dot_insert(alias, item.shallow_clone());
                        let mut member = Isolate::new();
                        member.set_environment(env.shallow_clone());
                        let value = member.run_compiled(code).map_err(|e| e.to_string())?;
                        env.dot_insert(key, value);
                    }
                }
                (Step::Table { keys, inputs, rules }, _) => {
                    let hit = self.hit(&mut isolate, inputs, rules)?;
                    let mut values = Vec::with_capacity(keys.len());
                    match hit {
                        Some(rule) => {
                            for code in &rule.values {
                                values.push(match code {
                                    Some(code) => isolate.run_compiled(code).map_err(|e| e.to_string())?,
                                    None => Variable::Null,
                                });
                            }
                        }
                        None => values.resize(keys.len(), Variable::Null),
                    }
                    for (key, value) in keys.iter().zip(values) {
                        self.write(&mut isolate, &input, &result, key, value);
                    }
                }
            }
        }
        let mut row = Vec::with_capacity(self.outputs.len());
        for target in &self.outputs {
            row.push(match &target.member {
                None => match self.dialect {
                    Dialect::Policy => input.dot(&target.name).map_or(Val::Null, |v| Val::of(&v)),
                    Dialect::Graph => result.dot(&target.name).map_or(Val::Null, |v| Val::of(&v)),
                },
                Some((list, field)) => match input.dot(list) {
                    Some(Variable::Array(items)) => Val::List(items.borrow().iter().map(|item| item.dot(field).map_or(Val::Null, |v| Val::of(&v))).collect()),
                    _ => Val::List(Vec::new()),
                },
            });
        }
        Ok(row)
    }

    fn write(&self, isolate: &mut Isolate, input: &Variable, result: &Variable, key: &str, value: Variable) {
        match self.dialect {
            Dialect::Policy => {
                input.dot_insert(key, value);
            }
            Dialect::Graph => {
                isolate.insert_dollar(key, value.clone());
                result.dot_insert(key, value);
            }
        }
    }
}
