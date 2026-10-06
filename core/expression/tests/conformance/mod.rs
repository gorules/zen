#![allow(dead_code)]

use rust_decimal::Decimal;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use zen_expression::{ExpressionKind, Isolate, Variable};

#[derive(Debug, Clone)]
pub enum Expected {
    Value(Variable),
    Error,
}

#[derive(Debug, Clone)]
pub struct SpecCase {
    pub file: String,
    pub line: usize,
    pub kind: ExpressionKind,
    pub expression: String,
    pub input: Option<Variable>,
    pub input_text: String,
    pub expected: Expected,
    pub expected_text: String,
    pub note: Note,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    None,
    Bug(String),
    Question(String),
}

impl SpecCase {
    pub fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }

    pub fn environment(&self) -> Variable {
        self.input
            .as_ref()
            .map_or(Variable::Null, |v| v.depth_clone(64))
    }

    pub fn stack(&self) -> Result<Variable, String> {
        Spec::guard(|| self.run_stack().map(Spec::settle))
    }

    fn run_stack(&self) -> Result<Variable, String> {
        let mut isolate = Isolate::new();
        if let Some(input) = &self.input {
            isolate.set_environment(input.depth_clone(64));
        }
        match self.kind {
            ExpressionKind::Standard => isolate
                .run_standard(&self.expression)
                .map_err(|e| e.to_string()),
            ExpressionKind::Unary => isolate
                .run_unary(&self.expression)
                .map(Variable::Bool)
                .map_err(|e| e.to_string()),
        }
    }

    pub fn verdict(&self, got: &Result<Variable, String>) -> Option<String> {
        match (&self.expected, got) {
            (Expected::Error, Err(e)) if !e.starts_with(Spec::PANIC) => None,
            (Expected::Value(want), Ok(v)) if Spec::same(want, v) => None,
            (_, got) => Some(format!(
                "{} | {} | input {} | expected {} | got {}",
                self.location(),
                self.expression,
                if self.input_text.is_empty() {
                    "-"
                } else {
                    &self.input_text
                },
                self.expected_text,
                match got {
                    Ok(v) => Spec::render(v),
                    Err(e) => format!("!error ({e})"),
                }
            )),
        }
    }
}

pub struct Spec;

impl Spec {
    pub const PANIC: &'static str = "PANIC";

    pub fn settle(v: Variable) -> Variable {
        let _ = v.to_value();
        v
    }

    pub fn guard(f: impl FnOnce() -> Result<Variable, String>) -> Result<Variable, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            Err(format!("{}: {message}", Self::PANIC))
        })
    }

    pub fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("spec")
    }

    pub fn prepare() {
        std::env::set_var(
            "TZ",
            std::env::var("SPEC_TZ").unwrap_or_else(|_| "UTC".to_string()),
        );
        std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-03-20T10:15:30Z");
    }

    pub fn files() -> Vec<(ExpressionKind, PathBuf)> {
        let filter = std::env::var("SPEC_FILE").ok();
        let mut out = Vec::new();
        for (dir, kind) in [
            ("standard", ExpressionKind::Standard),
            ("unary", ExpressionKind::Unary),
        ] {
            let Ok(entries) = std::fs::read_dir(Self::root().join(dir)) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "csv"))
                .filter(|p| {
                    filter
                        .as_ref()
                        .is_none_or(|f| p.to_string_lossy().contains(f.as_str()))
                })
                .collect();
            paths.sort();
            out.extend(paths.into_iter().map(|p| (kind.clone(), p)));
        }
        out
    }

    pub fn cases() -> (Vec<SpecCase>, Vec<String>) {
        let mut cases = Vec::new();
        let mut problems = Vec::new();
        for (kind, path) in Self::files() {
            let name = path
                .strip_prefix(Self::root())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    problems.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let lines: Vec<&str> = text.lines().collect();
            let mut reader = csv::ReaderBuilder::new()
                .delimiter(b';')
                .has_headers(false)
                .flexible(true)
                .comment(Some(b'#'))
                .from_reader(text.as_bytes());
            for record in reader.records() {
                let record = match record {
                    Ok(r) => r,
                    Err(e) => {
                        problems.push(format!("{name}: {e}"));
                        continue;
                    }
                };
                let mut line = record.position().map_or(0, |p| p.line() as usize);
                while lines
                    .get(line.wrapping_sub(1))
                    .is_some_and(|l| l.starts_with('#') || l.trim().is_empty())
                {
                    line += 1;
                }
                let field = |i: usize| record.get(i).unwrap_or_default().trim().to_string();
                let (expression, input_text, expected_text, note_text) =
                    (field(0), field(1), field(2), field(3));
                if expression.is_empty() || expression.starts_with("expression (") {
                    continue;
                }
                if record.len() < 3 {
                    problems.push(format!(
                        "{name}:{line}: expected 3 fields, got {}",
                        record.len()
                    ));
                    continue;
                }
                let input = match input_text.as_str() {
                    "" => None,
                    t => match Literal::parse(t) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            problems.push(format!("{name}:{line}: input {t}: {e}"));
                            continue;
                        }
                    },
                };
                let expected = match expected_text.as_str() {
                    t if t.starts_with("!error") => Expected::Error,
                    t => match Literal::parse(t) {
                        Ok(v) => Expected::Value(v),
                        Err(e) => {
                            problems.push(format!("{name}:{line}: output {t}: {e}"));
                            continue;
                        }
                    },
                };
                let note = match note_text.split_once(':') {
                    None if note_text.is_empty() => Note::None,
                    Some(("bug", text)) => Note::Bug(text.trim().to_string()),
                    Some(("question", text)) => Note::Question(text.trim().to_string()),
                    _ => {
                        problems.push(format!("{name}:{line}: note must be `bug: ...` or `question: ...`, got {note_text:?}"));
                        continue;
                    }
                };
                cases.push(SpecCase {
                    file: name.clone(),
                    line,
                    kind: kind.clone(),
                    expression,
                    input,
                    input_text,
                    expected,
                    expected_text,
                    note,
                });
            }
        }
        (cases, problems)
    }

    pub fn same(want: &Variable, got: &Variable) -> bool {
        match (want, got) {
            (Variable::Null, Variable::Null) => true,
            (Variable::Bool(a), Variable::Bool(b)) => a == b,
            (Variable::Number(a), Variable::Number(b)) => a == b,
            (Variable::String(a), Variable::String(b)) => a == b,
            (Variable::String(a), Variable::Dynamic(d)) => a.as_ref() == d.to_string(),
            (Variable::Dynamic(a), Variable::Dynamic(b)) => {
                a.type_name() == b.type_name() && a.to_string() == b.to_string()
            }
            (Variable::Array(a), Variable::Array(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Self::same(x, y))
            }
            (Variable::Object(a), Variable::Object(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, x)| b.get(k).is_some_and(|y| Self::same(x, y)))
            }
            _ => false,
        }
    }

    pub fn render(v: &Variable) -> String {
        match v {
            Variable::Null => "null".to_string(),
            Variable::Bool(b) => b.to_string(),
            Variable::Number(n) => n.to_string(),
            Variable::String(s) => Self::quote(s),
            Variable::Dynamic(d) => Self::quote(&d.to_string()),
            Variable::Array(a) => format!(
                "[{}]",
                a.borrow()
                    .iter()
                    .map(Self::render)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Variable::Object(o) => {
                let o = o.borrow();
                let mut entries: Vec<(String, String)> = o
                    .iter()
                    .map(|(k, v)| (k.to_string(), Self::render(v)))
                    .collect();
                entries.sort();
                format!(
                    "{{{}}}",
                    entries
                        .iter()
                        .map(|(k, v)| format!("{}: {v}", Self::quote(k)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }

    fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

pub struct Literal<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Literal<'a> {
    pub fn parse(text: &'a str) -> Result<Variable, String> {
        let mut p = Literal { text, at: 0 };
        let v = p.value()?;
        p.space();
        match p.at == p.text.len() {
            true => Ok(v),
            false => Err(format!("trailing input at {}", p.at)),
        }
    }

    fn rest(&self) -> &'a str {
        self.text.get(self.at..).unwrap_or_default()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: char) -> Result<(), String> {
        self.space();
        match self.peek() {
            Some(x) if x == c => {
                self.at += c.len_utf8();
                Ok(())
            }
            other => Err(format!("expected {c:?} at {}, found {other:?}", self.at)),
        }
    }

    fn value(&mut self) -> Result<Variable, String> {
        self.space();
        match self.peek() {
            Some('[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.space();
                    if self.peek() == Some(']') {
                        self.at += 1;
                        break;
                    }
                    items.push(self.value()?);
                    self.space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some(']') => {}
                        other => {
                            return Err(format!("expected , or ] at {}, found {other:?}", self.at))
                        }
                    }
                }
                Ok(Variable::from_array(items))
            }
            Some('{') => {
                self.at += 1;
                let object = Variable::empty_object();
                loop {
                    self.space();
                    if self.peek() == Some('}') {
                        self.at += 1;
                        break;
                    }
                    let key = match self.peek() {
                        Some('"' | '\'') => self.string()?,
                        _ => match self.word() {
                            w if w.is_empty() => {
                                return Err(format!("expected key at {}", self.at))
                            }
                            w => w,
                        },
                    };
                    self.eat(':')?;
                    let value = self.value()?;
                    if let Variable::Object(map) = &object {
                        map.borrow_mut().insert(key.as_str().into(), value);
                    }
                    self.space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some('}') => {}
                        other => {
                            return Err(format!("expected , or }} at {}, found {other:?}", self.at))
                        }
                    }
                }
                Ok(object)
            }
            Some('"' | '\'') => Ok(Variable::String(self.string()?.as_str().into())),
            Some(c) if c == '-' || c == '+' || c.is_ascii_digit() || c == '.' => self.number(),
            _ => match self.word().as_str() {
                "date" => {
                    self.eat('(')?;
                    self.space();
                    let text = self.string()?;
                    self.eat(')')?;
                    zen_expression::DateValue::from_text(&text)
                        .ok_or_else(|| format!("invalid date input {text:?}"))
                }
                "true" => Ok(Variable::Bool(true)),
                "false" => Ok(Variable::Bool(false)),
                "null" => Ok(Variable::Null),
                other => Err(format!("unexpected {other:?} at {}", self.at)),
            },
        }
    }

    fn word(&mut self) -> String {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            self.at += self.peek().map_or(1, char::len_utf8);
        }
        self.text
            .get(start..self.at)
            .unwrap_or_default()
            .to_string()
    }

    fn number(&mut self) -> Result<Variable, String> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E' | '_'))
        {
            self.at += 1;
        }
        let raw: String = self
            .text
            .get(start..self.at)
            .unwrap_or_default()
            .chars()
            .filter(|c| *c != '_' && *c != '+')
            .collect();
        let parsed = match raw.contains(['e', 'E']) {
            true => Decimal::from_scientific(&raw),
            false => Decimal::from_str(&raw),
        };
        parsed
            .map(Variable::Number)
            .map_err(|e| format!("number {raw:?}: {e}"))
    }

    fn string(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or("expected string")?;
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            self.at += c.len_utf8();
            match c {
                c if c == quote => return Ok(out),
                '\\' => {
                    let e = self.peek().ok_or("unterminated escape")?;
                    self.at += e.len_utf8();
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '0' => out.push('\0'),
                        'u' => {
                            let hex = self.text.get(self.at..self.at + 4).ok_or("short \\u")?;
                            let code = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                            out.push(char::from_u32(code).ok_or("bad \\u")?);
                            self.at += 4;
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
    }
}
