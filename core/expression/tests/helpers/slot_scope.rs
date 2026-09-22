use std::rc::Rc;
use zen_expression::variable::VariableType;

pub fn obj(fields: &[(&str, VariableType)]) -> VariableType {
    let object = VariableType::empty_object();
    if let VariableType::Object(map) = &object {
        for (key, value) in fields {
            map.borrow_mut().insert(Rc::from(*key), value.clone());
        }
    }
    object
}

fn enum_t(name: Option<&str>, values: &[&str]) -> VariableType {
    VariableType::Enum(
        name.map(Rc::from),
        values.iter().map(|v| Rc::from(*v)).collect(),
    )
}

pub fn status() -> VariableType {
    enum_t(Some("status"), &["open", "closed"])
}

pub fn array(inner: VariableType) -> VariableType {
    VariableType::Array(Rc::new(inner))
}

pub fn base_scope() -> VariableType {
    obj(&[
        ("age", VariableType::Number),
        ("name", VariableType::String),
        ("active", VariableType::Bool),
        ("since", VariableType::Date),
        (
            "amount",
            VariableType::Nullable(Rc::new(VariableType::Number)),
        ),
        ("status", status()),
        ("tier", enum_t(None, &["gold", "silver"])),
        ("mood", enum_t(Some("mood"), &["happy", "sad"])),
        (
            "customer",
            obj(&[
                ("age", VariableType::Number),
                ("name", VariableType::String),
                ("since", VariableType::Date),
                ("status", status()),
                ("address", obj(&[("city", VariableType::String)])),
            ]),
        ),
        (
            "items",
            array(obj(&[
                ("price", VariableType::Number),
                ("status", status()),
            ])),
        ),
        ("tags", array(VariableType::String)),
        ("statuses", array(status())),
        ("inbound", VariableType::Number),
        ("andrew", VariableType::String),
        ("nothing", VariableType::Bool),
        ("true_value", VariableType::Number),
        ("in_progress", VariableType::Bool),
        (
            "orders",
            array(obj(&[("sku", VariableType::String), ("status", status())])),
        ),
        ("grounding", obj(&[("status", status())])),
    ])
}

pub fn scope_for(spec: &str) -> VariableType {
    let subject = match spec {
        "" => return base_scope(),
        "$number" => VariableType::Number,
        "$string" => VariableType::String,
        "$date" => VariableType::Date,
        "$bool" => VariableType::Bool,
        "$any" => VariableType::Any,
        "$status" => status(),
        json => return serde_json::from_str(json).expect("scope json"),
    };
    let scope = base_scope();
    if let VariableType::Object(map) = &scope {
        map.borrow_mut().insert(Rc::from("$"), subject);
    }
    scope
}

pub fn expected_for(spec: &str) -> Option<VariableType> {
    Some(match spec {
        "" => return None,
        "bool" => VariableType::Bool,
        "number" => VariableType::Number,
        "string" => VariableType::String,
        "date" => VariableType::Date,
        "status" => status(),
        json => serde_json::from_str(json).expect("expected json"),
    })
}
