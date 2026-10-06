use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;
use zen_expression::Variable;

#[derive(Debug, Clone)]
pub enum Kind {
    Null,
    Bool(bool),
    Number(Decimal, String),
    Text(String),
    Date(String),
    List(Vec<Lit>),
    Map(Vec<(String, Lit)>),
}

#[derive(Debug, Clone)]
pub struct Lit {
    pub line: usize,
    pub kind: Kind,
}

impl Lit {
    pub fn get(&self, key: &str) -> Option<&Lit> {
        match &self.kind {
            Kind::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            Kind::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn list(&self) -> Option<&[Lit]> {
        match &self.kind {
            Kind::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn map(&self) -> Option<&[(String, Lit)]> {
        match &self.kind {
            Kind::Map(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key).map(|l| &l.kind), Some(Kind::Bool(true)))
    }

    pub fn variable(&self) -> Variable {
        match &self.kind {
            Kind::Null => Variable::Null,
            Kind::Bool(b) => Variable::Bool(*b),
            Kind::Number(n, _) => Variable::Number(*n),
            Kind::Text(s) => Variable::String(s.as_str().into()),
            Kind::Date(s) => zen_expression::DateValue::from_text(s).unwrap_or(Variable::Null),
            Kind::List(items) => Variable::from_array(items.iter().map(Lit::variable).collect()),
            Kind::Map(entries) => {
                let object = Variable::empty_object();
                if let Variable::Object(map) = &object {
                    let mut map = map.borrow_mut();
                    for (key, value) in entries {
                        map.insert(key.as_str().into(), value.variable());
                    }
                }
                object
            }
        }
    }

    pub fn json(&self) -> Value {
        match &self.kind {
            Kind::Null => Value::Null,
            Kind::Bool(b) => Value::Bool(*b),
            Kind::Number(n, raw) => serde_json::from_str(raw).or_else(|_| serde_json::from_str(&n.to_string())).unwrap_or(Value::Null),
            Kind::Text(s) | Kind::Date(s) => Value::String(s.clone()),
            Kind::List(items) => Value::Array(items.iter().map(Lit::json).collect()),
            Kind::Map(entries) => Value::Object(entries.iter().map(|(k, v)| (k.clone(), v.json())).collect()),
        }
    }

    pub fn source(&self) -> String {
        match &self.kind {
            Kind::Null => "null".to_string(),
            Kind::Bool(b) => b.to_string(),
            Kind::Number(_, raw) => raw.clone(),
            Kind::Text(s) => format!("{s:?}"),
            Kind::Date(s) => format!("date({s:?})"),
            Kind::List(items) => format!("[{}]", items.iter().map(Lit::source).collect::<Vec<_>>().join(", ")),
            Kind::Map(entries) => format!(
                "{{{}}}",
                entries.iter().map(|(k, v)| format!("{k}: {}", v.source())).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

pub struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    pub fn document(text: &'a str) -> Result<Vec<Lit>, String> {
        let mut p = Parser { text, at: 0 };
        let mut out = Vec::new();
        loop {
            p.space();
            if p.at >= p.text.len() {
                return Ok(out);
            }
            out.push(p.value().map_err(|e| format!("line {}: {e}", p.line()))?);
            p.space();
            if p.peek() == Some(',') {
                p.at += 1;
            }
        }
    }

    fn line(&self) -> usize {
        self.text.get(..self.at).unwrap_or_default().matches('\n').count() + 1
    }

    fn rest(&self) -> &'a str {
        self.text.get(self.at..).unwrap_or_default()
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn space(&mut self) {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.at += rest.len() - trimmed.len();
            if trimmed.starts_with("//") {
                self.at += trimmed.find('\n').unwrap_or(trimmed.len());
            } else if trimmed.starts_with("/*") {
                self.at += trimmed.find("*/").map_or(trimmed.len(), |end| end + 2);
            } else {
                return;
            }
        }
    }

    fn eat(&mut self, c: char) -> Result<(), String> {
        self.space();
        match self.peek() {
            Some(x) if x == c => {
                self.at += c.len_utf8();
                Ok(())
            }
            other => Err(format!("expected {c:?}, found {other:?}")),
        }
    }

    fn value(&mut self) -> Result<Lit, String> {
        self.space();
        let line = self.line();
        let kind = match self.peek() {
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
                        other => return Err(format!("expected , or ], found {other:?}")),
                    }
                }
                Kind::List(items)
            }
            Some('{') => {
                self.at += 1;
                let mut entries: Vec<(String, Lit)> = Vec::new();
                loop {
                    self.space();
                    if self.peek() == Some('}') {
                        self.at += 1;
                        break;
                    }
                    let key = match self.peek() {
                        Some('"' | '\'') => self.string()?,
                        _ => match self.word() {
                            w if w.is_empty() => return Err("expected key".to_string()),
                            w => w,
                        },
                    };
                    if entries.iter().any(|(k, _)| *k == key) {
                        return Err(format!("duplicate key {key:?}"));
                    }
                    self.eat(':')?;
                    let value = self.value()?;
                    entries.push((key, value));
                    self.space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some('}') => {}
                        other => return Err(format!("expected , or }}, found {other:?}")),
                    }
                }
                Kind::Map(entries)
            }
            Some('"') if self.rest().starts_with("\"\"\"") => Kind::Text(self.block()?),
            Some('"' | '\'') => Kind::Text(self.string()?),
            Some(c) if c == '-' || c == '+' || c.is_ascii_digit() || c == '.' => self.number()?,
            _ => match self.word().as_str() {
                "date" => {
                    self.eat('(')?;
                    self.space();
                    let text = self.string()?;
                    self.eat(')')?;
                    Kind::Date(text)
                }
                "true" => Kind::Bool(true),
                "false" => Kind::Bool(false),
                "null" => Kind::Null,
                other => return Err(format!("unexpected {other:?}")),
            },
        };
        Ok(Lit { line, kind })
    }

    fn word(&mut self) -> String {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            self.at += self.peek().map_or(1, char::len_utf8);
        }
        self.text.get(start..self.at).unwrap_or_default().to_string()
    }

    fn number(&mut self) -> Result<Kind, String> {
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
        parsed.map(|n| Kind::Number(n, raw.clone())).map_err(|e| format!("number {raw:?}: {e}"))
    }

    fn block(&mut self) -> Result<String, String> {
        self.at += 3;
        let rest = self.rest();
        let end = rest.find("\"\"\"").ok_or("unterminated \"\"\" string")?;
        let body = rest.get(..end).unwrap_or_default().to_string();
        self.at += end + 3;
        Ok(body)
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
