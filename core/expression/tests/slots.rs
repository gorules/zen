use std::rc::Rc;

use serde::Deserialize;
use zen_expression::intellisense::completion::Completions;
use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::{DateArg, LabelResolver, LiteralFact, Literals, Slot, SlotRole};
use zen_expression::variable::VariableType;

const BASE_SCOPE: &str = r#"{"Object":{
    "age":"Number","name":"String","active":"Bool","since":"Date",
    "amount":{"Nullable":"Number"},
    "status":{"Enum":["status",["open","closed"]]},
    "tier":{"Enum":[null,["gold","silver"]]},
    "mood":{"Enum":["mood",["happy","sad"]]},
    "grade":{"Enum":["grade",["a","b","c","d"]]},
    "customer":{"Object":{"age":"Number","name":"String","since":"Date",
        "status":{"Enum":["status",["open","closed"]]},
        "address":{"Object":{"city":"String"}}}},
    "items":{"Array":{"Object":{"price":"Number","status":{"Enum":["status",["open","closed"]]}}}},
    "tags":{"Array":"String"},
    "statuses":{"Array":{"Enum":["status",["open","closed"]]}},
    "grounding":{"Object":{"status":{"Enum":["status",["open","closed"]]}}}
}}"#;

#[derive(Deserialize)]
struct TestFile {
    test: Vec<TestCase>,
}

#[derive(Deserialize)]
struct TestCase {
    expression: String,
    role: Option<String>,
    subject: Option<String>,
    scope: Option<String>,
    expected: Option<String>,
    slot: Option<ExpectedSlot>,
    literals: Option<Vec<String>>,
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    excludes: Vec<String>,
}

#[derive(Deserialize)]
struct ExpectedSlot {
    state: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    options: Option<Vec<String>>,
    operators: Option<Vec<String>>,
    span: Option<String>,
    auto_open: Option<bool>,
}

impl TestCase {
    fn role(&self) -> SlotRole {
        match self.role.as_deref() {
            None if self.subject.is_some() => SlotRole::Unary,
            None | Some("condition") => SlotRole::Condition,
            Some("value") => SlotRole::Value,
            Some("path") => SlotRole::Path,
            Some(other) => panic!("unknown role {other}"),
        }
    }

    fn scope(&self) -> VariableType {
        let scope = parse_type(self.scope.as_deref().unwrap_or(BASE_SCOPE));
        if let (Some(subject), VariableType::Object(map)) = (&self.subject, &scope) {
            map.borrow_mut().insert(Rc::from("$"), parse_type(subject));
        }
        scope
    }

    fn expected(&self) -> Option<VariableType> {
        match (&self.expected, self.role()) {
            (Some(spec), _) => Some(parse_type(spec)),
            (None, SlotRole::Condition) => Some(VariableType::Bool),
            _ => None,
        }
    }

    fn check(&self, is: &mut IntelliSense) -> Vec<String> {
        let (source, pos) = match self.expression.find('|') {
            Some(caret) => (self.expression.replacen('|', "", 1), caret),
            None => (self.expression.clone(), self.expression.len()),
        };
        let scope = self.scope();
        let result = is.slot(
            &source,
            pos as u32,
            self.subject.is_some(),
            self.role(),
            &scope,
            self.expected().as_ref(),
        );
        let mut failures = Vec::new();
        if let Some(expected) = &self.slot {
            failures.extend(expected.check(&result.slot));
        }
        if let Some(expected) = &self.literals {
            let actual = render_literals(&source, &result.literals);
            if &actual != expected {
                failures.push(format!("literals: wanted {expected:?}, got {actual:?}"));
            }
        }
        if !self.includes.is_empty() || !self.excludes.is_empty() {
            let labels: Vec<String> =
                Completions::from_slot(&source, pos as u32, &scope, &result.slot)
                    .into_iter()
                    .map(|c| c.label)
                    .collect();
            for label in self.includes.iter().filter(|l| !labels.contains(l)) {
                failures.push(format!("completion {label} missing from {labels:?}"));
            }
            for label in self.excludes.iter().filter(|l| labels.contains(l)) {
                failures.push(format!("completion {label} unexpected in {labels:?}"));
            }
        }
        failures
    }
}

impl ExpectedSlot {
    fn check(&self, slot: &Slot) -> Vec<String> {
        let state = serde_json::to_value(slot.state).unwrap();
        let options: Vec<String> = slot
            .options
            .iter()
            .map(|o| match o.label == o.value {
                true => o.value.clone(),
                false => format!("{}={}", o.value, o.label),
            })
            .collect();
        let operators: Vec<String> = slot.operators.iter().map(|o| o.to_string()).collect();
        let fields = [
            (
                "state",
                Some(self.state.clone()),
                state.as_str().unwrap().to_string(),
            ),
            (
                "type",
                self.kind.clone(),
                slot.expected
                    .as_ref()
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
            ),
            (
                "options",
                self.options.as_ref().map(|o| o.join(",")),
                options.join(","),
            ),
            (
                "operators",
                self.operators.as_ref().map(|o| o.join(",")),
                operators.join(","),
            ),
            (
                "span",
                self.span.clone(),
                format!("{}..{}", slot.replace_span.0, slot.replace_span.1),
            ),
            (
                "auto_open",
                self.auto_open.map(|a| a.to_string()),
                slot.auto_open.to_string(),
            ),
        ];
        fields
            .into_iter()
            .filter_map(|(field, wanted, actual)| {
                let wanted = wanted?;
                (wanted != actual).then(|| format!("{field}: wanted {wanted:?}, got {actual:?}"))
            })
            .collect()
    }
}

fn parse_type(spec: &str) -> VariableType {
    let json = match spec {
        "bool" => r#""Bool""#,
        "number" => r#""Number""#,
        "string" => r#""String""#,
        "date" => r#""Date""#,
        "any" => r#""Any""#,
        "status" => r#"{"Enum":["status",["open","closed"]]}"#,
        "grade" => r#"{"Enum":["grade",["a","b","c","d"]]}"#,
        other => other,
    };
    serde_json::from_str(json).unwrap_or_else(|e| panic!("type {spec}: {e}"))
}

fn render_literals(source: &str, literals: &Literals) -> Vec<String> {
    literals
        .facts
        .iter()
        .map(|fact| {
            let span = fact.span();
            let text = &source[span.0 as usize..span.1 as usize];
            match fact {
                LiteralFact::Enum {
                    valid, enum_index, ..
                } => {
                    let options: Vec<String> = literals.enums[*enum_index as usize]
                        .options
                        .iter()
                        .map(|o| match o.label == o.value {
                            true => o.value.clone(),
                            false => format!("{}={}", o.value, o.label),
                        })
                        .collect();
                    let invalid = if *valid { "" } else { " invalid" };
                    format!("{text} enum {}{invalid}", options.join(","))
                }
                LiteralFact::Date { arg, .. } => match arg {
                    DateArg::Now => format!("{text} date now"),
                    DateArg::Today => format!("{text} date today"),
                    DateArg::Field { path } => format!("{text} date field {path}"),
                    DateArg::Literal { value, valid, tz } => {
                        let invalid = if *valid { "" } else { " invalid" };
                        let tz = tz.as_ref().map(|z| format!(" tz={z}")).unwrap_or_default();
                        format!("{text} date {value}{tz}{invalid}")
                    }
                },
                LiteralFact::Bool { value, .. } => format!("{text} bool {value}"),
            }
        })
        .collect()
}

fn labels() -> LabelResolver {
    Rc::new(|name: &str, value: &str| {
        let label = match (name, value) {
            ("status", "open") => "Open case",
            ("status", "closed") => "Closed",
            ("mood", "happy") => "😀 Happy",
            ("mood", "sad") => "😢 Sad",
            _ => return None,
        };
        Some(label.to_string())
    })
}

#[test]
fn slots() {
    let file: TestFile = toml::from_str(include_str!("data/slots.toml")).expect("slots.toml");
    let mut is = IntelliSense::new();
    is.set_labels(Some(labels()));
    let failures: Vec<String> = file
        .test
        .iter()
        .flat_map(|test| {
            test.check(&mut is)
                .into_iter()
                .map(move |f| format!("{}\n    {f}", test.expression))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn deeply_nested_lists_do_not_overflow_the_stack() {
    let scope = parse_type(BASE_SCOPE);
    for unary in [false, true] {
        let source = format!("{}a", "[".repeat(20_000));
        let handle = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                let scope = parse_type(BASE_SCOPE);
                let mut is = IntelliSense::new();
                is.slot(
                    &source,
                    source.len() as u32,
                    unary,
                    SlotRole::Value,
                    &scope,
                    None,
                );
                is.inspect(&source, source.len() as u32, unary, SlotRole::Value, &scope);
            })
            .unwrap();
        handle.join().expect("no stack overflow");
    }
    drop(scope);
}

#[test]
fn date_slots_offer_date_strings_except_in_comparisons() {
    let scope: VariableType =
        serde_json::from_str(r#"{"Object":{"since":"Date","appDate":"String","age":"Number"}}"#)
            .unwrap();
    let date = VariableType::Date;
    let labels = |source: &str, expected: Option<&VariableType>| -> Vec<String> {
        let mut is = IntelliSense::new();
        let pos = source.len() as u32;
        let result = is.slot(source, pos, false, SlotRole::Value, &scope, expected);
        Completions::from_slot(source, pos, &scope, &result.slot)
            .into_iter()
            .map(|c| c.label)
            .collect()
    };

    for (source, expected) in [
        ("d().isAfter(", None),
        ("", Some(&date)),
        ("app", Some(&date)),
    ] {
        let got = labels(source, expected);
        assert!(got.contains(&"appDate".to_string()), "{source:?}: {got:?}");
        assert!(!got.contains(&"age".to_string()), "{source:?}: {got:?}");
    }
    for source in [
        "since > ",
        "since == ",
        "since in [",
        "d().isAfter(appDate) and since < ",
    ] {
        let got = labels(source, None);
        assert!(!got.contains(&"appDate".to_string()), "{source:?}: {got:?}");
    }
}
