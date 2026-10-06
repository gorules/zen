#![allow(dead_code)]

pub mod graph;
pub mod literal;
pub mod policy;

use literal::{Kind, Lit, Parser};
use serde_json::Value;
use std::path::{Path, PathBuf};
use zen_expression::Variable;

#[derive(Debug, Clone)]
pub enum Expected {
    Value(Variable),
    Error(Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    None,
    Bug(String),
    Question(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dialect {
    Graph,
    Policy,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub line: usize,
    pub input: Variable,
    pub input_text: String,
    pub expected: Expected,
    pub expected_text: String,
    pub goals: Vec<String>,
    pub note: Note,
    pub engines: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Suite {
    pub file: String,
    pub line: usize,
    pub name: String,
    pub dialect: Dialect,
    pub content: Value,
    pub documents: Vec<(String, Value)>,
    pub diagnostics: Option<Vec<String>>,
    pub cases: Vec<Case>,
}

impl Suite {
    pub fn location(&self, case: &Case) -> String {
        format!("{}:{} [{}]", self.file, case.line, self.name)
    }

    pub fn verdict(&self, case: &Case, got: &Result<Variable, String>) -> Option<String> {
        let pass = match (&case.expected, got) {
            (Expected::Error(None), Err(_)) => true,
            (Expected::Error(Some(text)), Err(e)) => e.contains(text.as_str()),
            (Expected::Value(want), Ok(v)) => Spec::same(want, v),
            _ => false,
        };
        (!pass).then(|| {
            format!(
                "{} | input {} | expected {} | got {}",
                self.location(case),
                case.input_text,
                case.expected_text,
                match got {
                    Ok(v) => Spec::render(v),
                    Err(e) => format!("!error ({e})"),
                }
            )
        })
    }
}

pub struct Spec;

impl Spec {
    pub fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("spec")
    }

    pub fn prepare() {
        std::env::set_var("TZ", std::env::var("SPEC_TZ").unwrap_or_else(|_| "UTC".to_string()));
        std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-03-20T10:15:30Z");
    }

    pub fn files(dialect: Dialect) -> Vec<PathBuf> {
        let filter = std::env::var("SPEC_FILE").ok();
        let dir = match dialect {
            Dialect::Graph => "graph",
            Dialect::Policy => "policy",
        };
        let Ok(entries) = std::fs::read_dir(Self::root().join(dir)) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json5"))
            .filter(|p| filter.as_ref().is_none_or(|f| p.to_string_lossy().contains(f.as_str())))
            .collect();
        paths.sort();
        paths
    }

    pub fn suites(dialect: Dialect) -> (Vec<Suite>, Vec<String>) {
        let mut suites = Vec::new();
        let mut problems = Vec::new();
        let only = std::env::var("SPEC_SUITE").ok();
        for path in Self::files(dialect) {
            let file = path
                .strip_prefix(Self::root())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    problems.push(format!("{file}: {e}"));
                    continue;
                }
            };
            let document = match Parser::document(&text) {
                Ok(d) => d,
                Err(e) => {
                    problems.push(format!("{file}: {e}"));
                    continue;
                }
            };
            let mut names: Vec<String> = Vec::new();
            for lit in &document {
                match Self::suite(&file, dialect, lit) {
                    Ok(suite) => {
                        if names.contains(&suite.name) {
                            problems.push(format!("{file}:{}: duplicate suite name {:?}", lit.line, suite.name));
                        }
                        names.push(suite.name.clone());
                        if only.as_ref().is_none_or(|o| suite.name.contains(o.as_str())) {
                            suites.push(suite);
                        }
                    }
                    Err(e) => problems.push(format!("{file}:{}: {e}", lit.line)),
                }
            }
        }
        (suites, problems)
    }

    fn suite(file: &str, dialect: Dialect, lit: &Lit) -> Result<Suite, String> {
        let name = lit.get("name").and_then(Lit::text).ok_or("suite needs a `name`")?.to_string();
        let (main, extra, expand): (&str, &str, fn(&Lit) -> Result<Value, String>) = match dialect {
            Dialect::Graph => ("graph", "decisions", graph::Graph::expand),
            Dialect::Policy => ("policy", "policies", policy::Policy::expand),
        };
        let content = expand(lit.get(main).ok_or_else(|| format!("{name}: suite needs `{main}`"))?)
            .map_err(|e| format!("{name}: {e}"))?;
        let documents = lit
            .get(extra)
            .and_then(Lit::map)
            .unwrap_or_default()
            .iter()
            .map(|(key, doc)| {
                let expanded = match (dialect, doc.get("policy")) {
                    (_, Some(policy)) => policy::Policy::expand(policy),
                    (Dialect::Policy, None) if doc.get("graph").is_some() => doc.get("graph").map_or(Ok(Value::Null), graph::Graph::expand),
                    _ => expand(doc),
                };
                expanded.map(|v| (key.clone(), v)).map_err(|e| format!("{name}: {extra}.{key}: {e}"))
            })
            .collect::<Result<_, _>>()?;
        let cases = lit
            .get("cases")
            .and_then(Lit::list)
            .ok_or_else(|| format!("{name}: suite needs `cases`"))?
            .iter()
            .map(Self::case)
            .collect::<Result<_, _>>()
            .map_err(|e| format!("{name}: {e}"))?;
        let diagnostics = lit.get("diagnostics").and_then(Lit::list).map(|codes| {
            let mut codes: Vec<String> = codes.iter().filter_map(|c| c.text().map(str::to_string)).collect();
            codes.sort();
            codes
        });
        Ok(Suite {
            file: file.to_string(),
            line: lit.line,
            name,
            dialect,
            content,
            documents,
            diagnostics,
            cases,
        })
    }

    fn case(lit: &Lit) -> Result<Case, String> {
        let (input, output, note, goals) = match &lit.kind {
            Kind::List(parts) => match parts.as_slice() {
                [input, output] => (input, output, None, None),
                [input, output, note] => (input, output, Some(note), None),
                _ => return Err(format!("line {}: case must be [input, output, note?]", lit.line)),
            },
            Kind::Map(_) => (
                lit.get("input").ok_or_else(|| format!("line {}: case needs `input`", lit.line))?,
                lit.get("output").ok_or_else(|| format!("line {}: case needs `output`", lit.line))?,
                lit.get("note"),
                lit.get("goals"),
            ),
            _ => return Err(format!("line {}: case must be a list or object", lit.line)),
        };
        let expected = match output.text() {
            Some(text) if text.starts_with("!error") => {
                let detail = text.trim_start_matches("!error").trim_start_matches(':').trim();
                Expected::Error((!detail.is_empty()).then(|| detail.to_string()))
            }
            _ => Expected::Value(output.variable()),
        };
        let note_text = note.and_then(Lit::text).unwrap_or_default();
        let note = match note_text.split_once(':') {
            None if note_text.is_empty() => Note::None,
            Some(("bug", text)) => Note::Bug(text.trim().to_string()),
            Some(("question", text)) => Note::Question(text.trim().to_string()),
            _ => return Err(format!("line {}: note must be `bug: ...` or `question: ...`", lit.line)),
        };
        let engines = match &lit.kind {
            Kind::Map(_) => lit.get("engines").and_then(Lit::text).map(str::to_string),
            _ => None,
        };
        let goals = goals
            .and_then(Lit::list)
            .unwrap_or_default()
            .iter()
            .filter_map(|g| g.text().map(str::to_string))
            .collect();
        Ok(Case {
            line: lit.line,
            input: input.variable(),
            input_text: input.source(),
            expected,
            expected_text: output.source(),
            goals,
            engines,
            note,
        })
    }

    pub fn same(want: &Variable, got: &Variable) -> bool {
        match (want, got) {
            (Variable::Null, Variable::Null) => true,
            (Variable::Bool(a), Variable::Bool(b)) => a == b,
            (Variable::Number(a), Variable::Number(b)) => a == b,
            (Variable::String(a), Variable::String(b)) => a == b,
            (Variable::String(a), Variable::Dynamic(d)) => a.as_str() == d.to_string(),
            (Variable::Dynamic(a), Variable::Dynamic(b)) => a.type_name() == b.type_name() && a.to_string() == b.to_string(),
            (Variable::Array(a), Variable::Array(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Self::same(x, y))
            }
            (Variable::Object(a), Variable::Object(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).is_some_and(|y| Self::same(x, y)))
            }
            _ => false,
        }
    }

    pub fn render(v: &Variable) -> String {
        match v {
            Variable::Null => "null".to_string(),
            Variable::Bool(b) => b.to_string(),
            Variable::Number(n) => n.to_string(),
            Variable::String(s) => format!("{:?}", s.as_str()),
            Variable::Dynamic(d) => format!("{:?}", d.to_string()),
            Variable::Array(a) => format!("[{}]", a.borrow().iter().map(Self::render).collect::<Vec<_>>().join(", ")),
            Variable::Object(o) => {
                let o = o.borrow();
                let mut entries: Vec<(String, String)> = o.iter().map(|(k, v)| (k.to_string(), Self::render(v))).collect();
                entries.sort();
                format!(
                    "{{{}}}",
                    entries.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", ")
                )
            }
        }
    }
}
