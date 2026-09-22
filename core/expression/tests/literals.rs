use std::rc::Rc;

use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::{DateArg, EnumTable, LabelResolver, LiteralFact};
use zen_expression::variable::VariableType;

#[path = "helpers/slot_scope.rs"]
mod slot_scope;

use slot_scope::{expected_for, scope_for};

fn labels() -> LabelResolver {
    Rc::new(|name: &str, value: &str| match (name, value) {
        ("status", "open") => Some("Open case".to_string()),
        ("status", "closed") => Some("Closed".to_string()),
        _ => None,
    })
}

#[test]
fn date_literal_validity_uses_the_explicit_zone() {
    let (facts, _) = run("d(\"2024-09-08\", \"America/Santiago\")", false, "", "");
    assert!(facts[0].contains("invalid tz=America/Santiago"));
    let mut is = IntelliSense::new();
    let (_, _, complete) = is.literal_analysis("status == \"open\"", false, &scope_for(""), None);
    assert!(complete);
    let (_, _, complete) =
        is.literal_analysis("status == \"open\" and", false, &scope_for(""), None);
    assert!(!complete);
}

fn render(fact: &LiteralFact) -> String {
    match fact {
        LiteralFact::Enum {
            span,
            value,
            name,
            label,
            valid,
            enum_index,
        } => format!(
            "enum@{}..{} {value} {} #{enum_index} {label} {}",
            span.0,
            span.1,
            if *valid { "valid" } else { "invalid" },
            name.as_deref().unwrap_or("-")
        ),
        LiteralFact::Date { span, arg } => {
            let arg = match arg {
                DateArg::Now => "now".to_string(),
                DateArg::Today => "today".to_string(),
                DateArg::Literal { value, valid, tz } => format!(
                    "literal {value} {}{}",
                    if *valid { "valid" } else { "invalid" },
                    tz.as_ref()
                        .map(|tz| format!(" tz={tz}"))
                        .unwrap_or_default()
                ),
                DateArg::Field { path } => format!("field {path}"),
            };
            format!("date@{}..{} {arg}", span.0, span.1)
        }
        LiteralFact::Bool { span, value } => format!("bool@{}..{} {value}", span.0, span.1),
    }
}

fn run(source: &str, unary: bool, scope: &str, expected: &str) -> (Vec<String>, Vec<EnumTable>) {
    let mut is = IntelliSense::new();
    is.set_labels(Some(labels()));
    let (facts, enums) = is.literals(
        source,
        unary,
        &scope_for(scope),
        expected_for(expected).as_ref(),
    );
    (facts.iter().map(render).collect(), enums)
}

struct Case {
    source: &'static str,
    unary: bool,
    scope: &'static str,
    expected: &'static str,
    facts: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        source: "status == \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 open valid #0 Open case status"],
    },
    Case {
        source: "status == \"nope\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 nope invalid #0 nope status"],
    },
    Case {
        source: "\"open\" == status",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@0..6 open valid #0 Open case status"],
    },
    Case {
        source: "status in [\"open\", \"closed\"]",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@11..17 open valid #0 Open case status",
            "enum@19..27 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "status not in [\"open\", \"x\"]",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@15..21 open valid #0 Open case status",
            "enum@23..26 x invalid #0 x status",
        ],
    },
    Case {
        source: "tier == \"gold\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@8..14 gold valid #0 gold -"],
    },
    Case {
        source: "customer.status == \"closed\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@19..27 closed valid #0 Closed status"],
    },
    Case {
        source: "statuses[0] == \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@15..21 open valid #0 Open case status"],
    },
    Case {
        source: "since > d()",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..11 now"],
    },
    Case {
        source: "since > d(\"2024-01-01\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..23 literal 2024-01-01 valid"],
    },
    Case {
        source: "since > d(\"not a date\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..23 literal not a date invalid"],
    },
    Case {
        source: "since > d(customer.since)",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..25 field customer.since"],
    },
    Case {
        source: "d()",
        unary: false,
        scope: "",
        expected: "",
        facts: &["date@0..3 now"],
    },
    Case {
        source: "d(\"2024-01-01\", \"Europe/Berlin\")",
        unary: false,
        scope: "",
        expected: "",
        facts: &["date@0..32 literal 2024-01-01 valid tz=Europe/Berlin"],
    },
    Case {
        source: "active == true",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@10..14 true"],
    },
    Case {
        source: "true",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@0..4 true"],
    },
    Case {
        source: "active and false",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@11..16 false"],
    },
    Case {
        source: "age > 1 ? true : false",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@10..14 true", "bool@17..22 false"],
    },
    Case {
        source: "\"open\", \"closed\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@0..6 open valid #0 Open case status",
            "enum@8..16 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "\"open\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@0..6 open valid #0 Open case status"],
    },
    Case {
        source: "[\"open\", \"x\"]",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@1..7 open valid #0 Open case status",
            "enum@9..12 x invalid #0 x status",
        ],
    },
    Case {
        source: "!= \"closed\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@3..11 closed valid #0 Closed status"],
    },
    Case {
        source: "\"open\" or \"closed\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@0..6 open valid #0 Open case status",
            "enum@10..18 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "> d(\"2024-01-01\")",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@2..17 literal 2024-01-01 valid"],
    },
    Case {
        source: "[1..5]",
        unary: true,
        scope: "$number",
        expected: "",
        facts: &[],
    },
    Case {
        source: "true",
        unary: true,
        scope: "$bool",
        expected: "",
        facts: &["bool@0..4 true"],
    },
    Case {
        source: "since in [d(\"2024-01-01\")..d()]",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@10..25 literal 2024-01-01 valid", "date@27..30 now"],
    },
    Case {
        source: "some(items, #.status == \"open\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@24..30 open valid #0 Open case status"],
    },
    Case {
        source: "some([\"open\", \"x\"], # in statuses)",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@6..12 open valid #0 Open case status",
            "enum@14..17 x invalid #0 x status",
        ],
    },
    Case {
        source: "contains(statuses, \"open\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@19..25 open valid #0 Open case status"],
    },
    Case {
        source: "since.isAfter(d(\"2024-01-01\"))",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@14..29 literal 2024-01-01 valid"],
    },
    Case {
        source: "age > 18 ? \"open\" : \"closed\"",
        unary: false,
        scope: "",
        expected: "status",
        facts: &[
            "enum@11..17 open valid #0 Open case status",
            "enum@20..28 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "status ?? \"x\"",
        unary: false,
        scope: "",
        expected: "status",
        facts: &["enum@10..13 x invalid #0 x status"],
    },
    Case {
        source: "\"open\"",
        unary: false,
        scope: "",
        expected: "status",
        facts: &["enum@0..6 open valid #0 Open case status"],
    },
    Case {
        source: "status == \"op",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "something == \"hello\"",
        unary: false,
        scope: "{\"Object\":{\"something\":{\"Const\":\"hello\"}}}",
        expected: "bool",
        facts: &["enum@13..20 hello valid #0 hello -"],
    },
    Case {
        source: "something == \"helloo\"",
        unary: false,
        scope: "{\"Object\":{\"something\":{\"Const\":\"hello\"}}}",
        expected: "bool",
        facts: &["enum@13..21 helloo invalid #0 helloo -"],
    },
    Case {
        source: "\"hello\" == something",
        unary: false,
        scope: "{\"Object\":{\"something\":{\"Const\":\"hello\"}}}",
        expected: "bool",
        facts: &["enum@0..7 hello valid #0 hello -"],
    },
    Case {
        source: "something in [\"hello\", \"x\"]",
        unary: false,
        scope: "{\"Object\":{\"something\":{\"Nullable\":{\"Const\":\"hello\"}}}}",
        expected: "bool",
        facts: &[
            "enum@14..21 hello valid #0 hello -",
            "enum@23..26 x invalid #0 x -",
        ],
    },
    Case {
        source: "something == \"b\"",
        unary: false,
        scope: "{\"Object\":{\"something\":{\"Enum\":[null,[\"a\",\"b\"]]}}}",
        expected: "bool",
        facts: &["enum@13..16 b valid #0 b -"],
    },
    Case {
        source: "\"open\" == \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "`hello ${status}`",
        unary: false,
        scope: "",
        expected: "",
        facts: &[],
    },
    Case {
        source: "customer.name == \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "name == \"Ünïcode\" and status == \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@34..40 open valid #0 Open case status"],
    },
    Case {
        source: "since > d(\"2024-01-01T10:00:00Z\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..33 literal 2024-01-01T10:00:00Z valid"],
    },
    Case {
        source: "since > d(\"2024-01-01 10:00\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..29 literal 2024-01-01 10:00 valid"],
    },
    Case {
        source: "since > d(\"2024-13-01\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..23 literal 2024-13-01 invalid"],
    },
    Case {
        source: "since > d(\"2024-02-30\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..23 literal 2024-02-30 invalid"],
    },
    Case {
        source: "since > d(\"today\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..18 literal today invalid"],
    },
    Case {
        source: "since > d(\"\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..13 literal  invalid"],
    },
    Case {
        source: "since > d(\"Europe/Berlin\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..26 literal Europe/Berlin valid"],
    },
    Case {
        source: "since > d(1700000000)",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "d(\"2026-09-19 10:00:00\", \"Europe/Berlin\")",
        unary: false,
        scope: "",
        expected: "",
        facts: &["date@0..41 literal 2026-09-19 10:00:00 valid tz=Europe/Berlin"],
    },
    Case {
        source: "since > d(\"2026-09-19 10:00:00\", \"Europe/Berlin\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..49 literal 2026-09-19 10:00:00 valid tz=Europe/Berlin"],
    },
    Case {
        source: "> d('2026-09-19 10:00:00', 'America/New_York')",
        unary: true,
        scope: "",
        expected: "",
        facts: &["date@2..46 literal 2026-09-19 10:00:00 valid tz=America/New_York"],
    },
    Case {
        source: "since > d(\"2026-09-19 10:00:00\", \"Mars/Olympus\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..48 literal 2026-09-19 10:00:00 invalid tz=Mars/Olympus"],
    },
    Case {
        source: "since > d(\"not a date\", \"Europe/Berlin\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..40 literal not a date invalid tz=Europe/Berlin"],
    },
    Case {
        source: "since > d(\"2026-09-19 10:00:00\", customer.zone)",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "d(\"2024-01-01\") < since",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@0..15 literal 2024-01-01 valid"],
    },
    Case {
        source: "d().startOf(\"day\")",
        unary: false,
        scope: "",
        expected: "",
        facts: &["date@0..18 today"],
    },
    Case {
        source: "since > d().startOf(\"day\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@8..26 today"],
    },
    Case {
        source: "> d().startOf('day')",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@2..20 today"],
    },
    Case {
        source: "d().startOf(\"month\") < since",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@0..3 now"],
    },
    Case {
        source: "d().add(1, \"day\") > since",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@0..3 now"],
    },
    Case {
        source: "since.add(1, \"day\").isAfter(d())",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@28..31 now"],
    },
    Case {
        source: "since.diff(d(\"2024-01-01\"), \"days\") > 1",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@11..26 literal 2024-01-01 valid"],
    },
    Case {
        source: "since in (d(\"2024-01-01\")..d(\"2024-12-31\"))",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "date@10..25 literal 2024-01-01 valid",
            "date@27..42 literal 2024-12-31 valid",
        ],
    },
    Case {
        source: "d(customer.since).isAfter(d(\"2024-01-01\"))",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "date@0..17 field customer.since",
            "date@26..41 literal 2024-01-01 valid",
        ],
    },
    Case {
        source: "d(items[0].price) > since",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@0..17 field items[0].price"],
    },
    Case {
        source: "> d($)",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@2..6 field $"],
    },
    Case {
        source: "> d($).startOf(\"day\")",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@2..6 field $"],
    },
    Case {
        source: "(status ?? \"open\") == \"closed\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@11..17 open valid #0 Open case status",
            "enum@22..30 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "status ?? \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 open valid #0 Open case status"],
    },
    Case {
        source: "age > 18 ? \"open\" : active ? \"closed\" : \"x\"",
        unary: false,
        scope: "",
        expected: "status",
        facts: &[
            "enum@11..17 open valid #0 Open case status",
            "enum@29..37 closed valid #0 Closed status",
            "enum@40..43 x invalid #0 x status",
        ],
    },
    Case {
        source: "count(items, #.status in [\"open\", \"x\"]) > 0",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@26..32 open valid #0 Open case status",
            "enum@34..37 x invalid #0 x status",
        ],
    },
    Case {
        source: "filter(items, #.status == \"closed\")",
        unary: false,
        scope: "",
        expected: "",
        facts: &["enum@26..34 closed valid #0 Closed status"],
    },
    Case {
        source: "map(items, #.status == \"open\")",
        unary: false,
        scope: "",
        expected: "",
        facts: &["enum@23..29 open valid #0 Open case status"],
    },
    Case {
        source: "some(items, #.status == \"open\" or #.status == \"x\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@24..30 open valid #0 Open case status",
            "enum@46..49 x invalid #0 x status",
        ],
    },
    Case {
        source: "[\"open\", \"x\"]",
        unary: false,
        scope: "",
        expected: "{\"Array\":{\"Enum\":[\"status\",[\"open\",\"closed\"]]}}",
        facts: &[
            "enum@1..7 open valid #0 Open case status",
            "enum@9..12 x invalid #0 x status",
        ],
    },
    Case {
        source: "\"open\" in statuses",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@0..6 open valid #0 Open case status"],
    },
    Case {
        source: "tier in [\"gold\", \"bronze\"]",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@9..15 gold valid #0 gold -",
            "enum@17..25 bronze invalid #0 bronze -",
        ],
    },
    Case {
        source: "items[0].status == \"open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@19..25 open valid #0 Open case status"],
    },
    Case {
        source: "customer.status in [\"open\"] and mood == \"happy\" and status == \"closed\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@20..26 open valid #0 Open case status",
            "enum@40..47 happy valid #1 happy mood",
            "enum@62..70 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "status == \"open\" ? true : false",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@10..16 open valid #0 Open case status",
            "bool@19..23 true",
            "bool@26..31 false",
        ],
    },
    Case {
        source: "contains(name, \"open\")",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "`${status == \"open\"}`",
        unary: false,
        scope: "",
        expected: "",
        facts: &["enum@13..19 open valid #0 Open case status"],
    },
    Case {
        source: "{ status: \"open\" }",
        unary: false,
        scope: "",
        expected: "{\"Object\":{\"status\":{\"Enum\":[\"status\",[\"open\",\"closed\"]]}}}",
        facts: &["enum@10..16 open valid #0 Open case status"],
    },
    Case {
        source: "status == 'open'",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 open valid #0 Open case status"],
    },
    Case {
        source: "status == \"Open\"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 Open invalid #0 Open status"],
    },
    Case {
        source: "active == false",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@10..15 false"],
    },
    Case {
        source: "true == active",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@0..4 true"],
    },
    Case {
        source: "not active",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "age == true",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[],
    },
    Case {
        source: "[true, false]",
        unary: false,
        scope: "",
        expected: "{\"Array\":\"Bool\"}",
        facts: &["bool@1..5 true", "bool@7..12 false"],
    },
    Case {
        source: "== true",
        unary: true,
        scope: "$bool",
        expected: "",
        facts: &["bool@3..7 true"],
    },
    Case {
        source: "in [\"open\", \"x\"]",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@4..10 open valid #0 Open case status",
            "enum@12..15 x invalid #0 x status",
        ],
    },
    Case {
        source: "not in [\"closed\"]",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@8..16 closed valid #0 Closed status"],
    },
    Case {
        source: "$ == \"open\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@5..11 open valid #0 Open case status"],
    },
    Case {
        source: "== \"x\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@3..6 x invalid #0 x status"],
    },
    Case {
        source: "\"open\" or \"x\"",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@0..6 open valid #0 Open case status",
            "enum@10..13 x invalid #0 x status",
        ],
    },
    Case {
        source: "\"open\", \"clo",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@0..6 open valid #0 Open case status"],
    },
    Case {
        source: "\"open\"",
        unary: true,
        scope: "$string",
        expected: "",
        facts: &[],
    },
    Case {
        source: "d(\"2024-01-01\") a",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@0..15 literal 2024-01-01 valid"],
    },
    Case {
        source: "d(\"\") a",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@0..5 literal  invalid"],
    },
    Case {
        source: "d() and",
        unary: true,
        scope: "$date",
        expected: "",
        facts: &["date@0..3 now"],
    },
    Case {
        source: "\"open\", \"closed\" a",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@0..6 open valid #0 Open case status",
            "enum@8..16 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "\"open\" or",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &["enum@0..6 open valid #0 Open case status"],
    },
    Case {
        source: "status in [\"open\", \"closed\"] ?",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@11..17 open valid #0 Open case status",
            "enum@19..27 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "status == \"open\" and (",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 open valid #0 Open case status"],
    },
    Case {
        source: "status == \"open\" ? \"closed\" : \"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@10..16 open valid #0 Open case status"],
    },
    Case {
        source: "status == \"open\" ? \"closed\" : \"",
        unary: false,
        scope: "",
        expected: "status",
        facts: &[
            "enum@10..16 open valid #0 Open case status",
            "enum@19..27 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "tier in [\"gold\", \"silver\"] and customer.age >",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@9..15 gold valid #0 gold -",
            "enum@17..25 silver valid #0 silver -",
        ],
    },
    Case {
        source: "[\"open\", \"closed\", ",
        unary: true,
        scope: "$status",
        expected: "",
        facts: &[
            "enum@1..7 open valid #0 Open case status",
            "enum@9..17 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "[\"open\", \"closed\", ",
        unary: false,
        scope: "",
        expected: "{\"Array\":{\"Enum\":[\"status\",[\"open\",\"closed\"]]}}",
        facts: &[
            "enum@1..7 open valid #0 Open case status",
            "enum@9..17 closed valid #0 Closed status",
        ],
    },
    Case {
        source: "(status == \"open\" and (",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["enum@11..17 open valid #0 Open case status"],
    },
    Case {
        source: "active == true and (",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["bool@10..14 true"],
    },
    Case {
        source: "since.add(1, \"day\") > d() and (",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &["date@22..25 now"],
    },
    Case {
        source: "status == \"open\" and since > d(\"2024-01-01\") and mood == \"",
        unary: false,
        scope: "",
        expected: "bool",
        facts: &[
            "enum@10..16 open valid #0 Open case status",
            "date@29..44 literal 2024-01-01 valid",
        ],
    },
];

#[test]
fn literal_facts() {
    let mut failures = Vec::new();
    for case in CASES {
        let (facts, _) = run(case.source, case.unary, case.scope, case.expected);
        let wanted: Vec<String> = case.facts.iter().map(|f| f.to_string()).collect();
        if facts != wanted {
            failures.push(format!(
                "{}\n    wanted {:?}\n    got    {:?}",
                case.source, wanted, facts
            ));
        }
    }
    assert!(CASES.len() >= 25);
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        CASES.len(),
        failures.join("\n")
    );
}

#[test]
fn incomplete_tails_keep_earlier_facts() {
    let mut failures = Vec::new();
    for case in CASES {
        let (complete, _) = run(case.source, case.unary, case.scope, case.expected);
        if complete.is_empty() || case.source.ends_with('"') && !case.source.ends_with("\"\"") {
            continue;
        }
        let tails: &[&str] = if case.unary {
            &[" a", " and", ", ", " or ("]
        } else {
            &[" and (", " ?", " and", " or (", " and customer.age >"]
        };
        for tail in tails {
            let source = format!("{}{tail}", case.source);
            let (facts, _) = run(&source, case.unary, case.scope, case.expected);
            if facts != complete {
                failures.push(format!(
                    "{source}\n    wanted {complete:?}\n    got    {facts:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} tails failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn enum_tables_are_deduplicated() {
    let (facts, enums) = run(
        "status == \"open\" or mood == \"happy\" or status == \"closed\"",
        false,
        "",
        "bool",
    );
    assert_eq!(facts.len(), 3);
    assert_eq!(enums.len(), 2);
    assert_eq!(enums[0].name.as_deref(), Some("status"));
    assert_eq!(enums[0].options[0].label, "Open case");
    assert_eq!(enums[0].options[0].source.as_deref(), Some("\"open\""));
    assert_eq!(enums[1].name.as_deref(), Some("mood"));
    assert_eq!(enums[1].options[0].label, "happy");
    assert!(facts[1].contains("#1"));
    assert!(facts[2].contains("#0"));
}

#[test]
fn anonymous_enum_table_has_no_name() {
    let (_, enums) = run("tier == \"gold\"", false, "", "bool");
    assert_eq!(enums.len(), 1);
    assert_eq!(enums[0].name, None);
    assert_eq!(
        enums[0]
            .options
            .iter()
            .map(|o| o.value.as_str())
            .collect::<Vec<_>>(),
        vec!["gold", "silver"]
    );
}

#[test]
fn const_literal_has_anonymous_table() {
    let scope = "{\"Object\":{\"something\":{\"Const\":\"hello\"}}}";
    let (facts, enums) = run("something == \"hello\"", false, scope, "bool");
    assert_eq!(
        facts,
        vec!["enum@13..20 hello valid #0 hello -".to_string()]
    );
    assert_eq!(enums.len(), 1);
    assert_eq!(enums[0].name, None);
    assert_eq!(enums[0].options.len(), 1);
    assert_eq!(enums[0].options[0].value, "hello");
    assert_eq!(enums[0].options[0].label, "hello");
    assert_eq!(enums[0].options[0].source.as_deref(), Some("\"hello\""));

    let (facts, enums) = run("something == \"helloo\"", false, scope, "bool");
    assert_eq!(
        facts,
        vec!["enum@13..21 helloo invalid #0 helloo -".to_string()]
    );
    assert_eq!(enums.len(), 1);
    assert_eq!(enums[0].options[0].value, "hello");
}

#[test]
fn facts_without_labels_fall_back_to_values() {
    let mut is = IntelliSense::new();
    let (facts, enums) = is.literals(
        "status == \"open\"",
        false,
        &scope_for(""),
        Some(&VariableType::Bool),
    );
    assert_eq!(render(&facts[0]), "enum@10..16 open valid #0 open status");
    assert_eq!(enums[0].options[1].label, "closed");
}
