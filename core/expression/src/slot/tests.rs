use std::rc::Rc;

use super::operators::operators_for;
use super::{encode_string, enum_options, LabelResolver};
use crate::variable::VariableType;

#[test]
fn encode_string_picks_a_quote() {
    assert_eq!(encode_string("open").as_deref(), Some("\"open\""));
    assert_eq!(encode_string("say \"hi\"").as_deref(), Some("'say \"hi\"'"));
    assert_eq!(encode_string("it's \"x\""), None);
}

#[test]
fn enum_options_resolve_labels() {
    let labels: LabelResolver = Rc::new(|name: &str, value: &str| {
        (name == "status" && value == "open").then(|| "Open case".to_string())
    });
    let values = [Rc::from("open"), Rc::from("closed")];
    let options = enum_options(Some("status"), &values, Some(&labels));
    assert_eq!(options[0].label, "Open case");
    assert_eq!(options[0].value, "open");
    assert_eq!(options[0].source.as_deref(), Some("\"open\""));
    assert_eq!(options[1].label, "closed");

    let unlabeled = enum_options(None, &values, Some(&labels));
    assert_eq!(unlabeled[0].label, "open");
}

#[test]
fn operators_by_type() {
    assert_eq!(
        operators_for(&VariableType::Number, false),
        vec!["==", "!=", "<", "<=", ">", ">=", "in", "not in"]
    );
    assert_eq!(
        operators_for(&VariableType::Date, true),
        vec![">", ">=", "<", "<=", "==", "!=", "in", "not in"]
    );
    let status = VariableType::Enum(None, vec![Rc::from("a")]);
    assert_eq!(
        operators_for(&status, false),
        vec!["==", "!=", "in", "not in"]
    );
    assert_eq!(operators_for(&status, true), vec!["!=", "in", "not in"]);
    assert_eq!(operators_for(&VariableType::Bool, true), vec!["==", "!="]);
    assert_eq!(
        operators_for(&VariableType::Array(Rc::new(VariableType::String)), false),
        vec!["in", "not in"]
    );
    assert_eq!(
        operators_for(
            &VariableType::Nullable(Rc::new(VariableType::Number)),
            false
        ),
        vec!["==", "!=", "<", "<=", ">", ">=", "in", "not in", "??"]
    );
    assert_eq!(
        operators_for(&VariableType::Nullable(Rc::new(VariableType::Bool)), false),
        vec!["==", "!=", "??"]
    );
    assert_eq!(
        operators_for(&VariableType::Nullable(Rc::new(status.clone())), true),
        vec!["==", "!=", "in", "not in"]
    );
    assert_eq!(
        operators_for(&VariableType::Const(Rc::from("hello")), false),
        vec!["==", "!=", "in", "not in"]
    );
}
