use crate::conformance;
use crate::policy_gen::{Gen, Rng};
use crate::support::columns::Built;
use crate::support::golden::Canon;
use serde::Deserialize;
use serde_json::{json, Value};
use zen_engine::policy::PolicyWorkspace;
use zen_expression::Variable;

pub struct Batch {
    pub suite: &'static str,
    pub policy: String,
    pub values: Vec<Value>,
}

impl Batch {
    pub fn new(suite: &'static str, policy: &str, values: Vec<Value>) -> Batch {
        Batch {
            suite,
            policy: policy.to_string(),
            values,
        }
    }

    pub fn workspace(&self) -> PolicyWorkspace {
        Anchor::workspace_with(&self.policy)
    }

    pub fn column_rows(&self) -> Vec<Variable> {
        let mut out = Vec::new();
        if let Some(built) = Built::new(&self.values) {
            let columns = built.columns();
            out.extend((0..columns.rows).map(|row| columns.row(row)));
        }
        if let Some(built) = Built::nested(&self.values) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            out.extend((0..columns.rows).map(|row| columns.row(row)));
        }
        out
    }
}

pub struct Anchor;

impl Anchor {
    pub const PATH: &'static str = "anchor";

    pub const POLICY: &'static str = r#"{
  models: {
    customer: {age: "number", income: "number", tier: "string", rate: "number", accounts: "account[]"},
    account: {balance: "number", kind: "string", active: "boolean"},
  },
  blocks: [
    {expression: "account.fee", value: "account.balance * customer.rate / 100 * (account.kind == 'loan' ? 2 : 1)"},
    {match: "account.label", arms: [["account.kind == 'savings'", "'S'"], ["account.kind == 'checking'", "'C'"], ["", "'L'"]]},
    {expression: "customer.total", value: "sum(map(filter(customer.accounts, #.active), #.balance))"},
    {expression: "customer.fees", value: "sum(map(customer.accounts, #.fee))"},
    {match: "customer.risk", arms: [["customer.total > customer.income * 2", "'high'"], ["customer.total > customer.income", "'mid'"], ["", "'low'"]]},
    {table: "discount", inputs: ["customer.tier", "customer.risk"], outputs: ["customer.discount"], rules: [
      ["'gold'", "'low'", "0.15"],
      ["'gold'", "'mid'", "0.1"],
      ["'silver'", "'low'", "0.1"],
      ["'silver'", "'mid'", "0.05"],
      ["", "'low'", "0.02"],
      ["", "", "0"],
    ]},
    {expression: "customer.net", value: "customer.total - customer.fees - customer.discount"},
    {assertion: "customer.eligible", conditions: ["customer.age >= 18", "customer.risk != 'high'"]},
  ],
}"#;

    pub fn rows(count: usize, seed: u64) -> Vec<Value> {
        let mut state = seed | 1;
        let mut pick = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        (0..count)
            .map(|_| {
                let accounts: Vec<Value> = (0..pick(8))
                    .map(|_| {
                        let (balance, kind, active) = (pick(5000), ["savings", "checking", "loan"][pick(3) as usize], pick(4) != 0);
                        json!({"balance": balance, "kind": kind, "active": active})
                    })
                    .collect();
                let (age, income, tier, rate) = (16 + pick(64), 1000 + pick(9000), ["gold", "silver", "bronze"][pick(3) as usize], 1 + pick(5));
                json!({"customer": {"age": age, "income": income, "tier": tier, "rate": rate, "accounts": accounts}})
            })
            .collect()
    }

    pub fn workspace() -> PolicyWorkspace {
        Self::workspace_with(Self::POLICY)
    }

    pub fn workspace_with(policy: &str) -> PolicyWorkspace {
        let literal = conformance::literal::Parser::document(policy).expect("policy literal");
        let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("anchor", serde_json::from_value(document).expect("policy json"));
        ws
    }

    pub fn typed() -> bool {
        std::env::var("ZEN_TYPED").map_or(true, |v| v.trim() != "0")
    }

    pub fn base() -> Batch {
        Batch::new("anchor", Self::POLICY, Self::rows(1024, 7))
    }

    pub fn fallback_policy() -> String {
        Self::POLICY.replace(
            "    {match: \"customer.risk\"",
            "    {expression: \"customer.label\", value: \"split(customer.tier, 'l')\"},\n    {match: \"customer.risk\"",
        )
    }

    pub fn fallback() -> Batch {
        Batch::new("anchor-fallback", &Self::fallback_policy(), Self::rows(256, 5))
    }

    pub fn fused_policy() -> String {
        Self::POLICY.replace(
            "    {match: \"customer.risk\"",
            "    {expression: \"customer.label\", value: \"split(customer.tier, 'l')\"},\n    {expression: \"customer.tag\", value: \"split(customer.tier, 'o')\"},\n    {match: \"customer.risk\"",
        )
    }

    pub fn fusion(seed: u64) -> Batch {
        Batch::new("anchor-fused", &Self::fused_policy(), Self::rows(256, seed))
    }

    pub fn members_policy() -> String {
        Self::POLICY
            .replace(
                "    {expression: \"customer.total\"",
                "    {table: \"band\", inputs: [\"account.balance\", \"customer.tier\"], outputs: [\"account.band\", \"account.boost\"], rules: [\n      [\"> 4000\", \"'gold'\", \"'A'\", \"1.5\"],\n      [\"> 2000\", \"\", \"'B'\", \"2\"],\n      [\"\", \"\", \"'C'\", \"0\"],\n    ]},\n    {expression: \"customer.total\"",
            )
            .replace(
                "    {assertion: \"customer.eligible\"",
                "    {expression: \"customer.rich\", value: \"some(customer.accounts, #.balance > 4000)\"},\n    {expression: \"customer.active\", value: \"all(customer.accounts, #.active)\"},\n    {expression: \"customer.loans\", value: \"count(customer.accounts, #.kind == 'loan')\"},\n    {expression: \"customer.third\", value: \"round(customer.total / 3, 2) + customer.total % 7\"},\n    {expression: \"customer.gap\", value: \"abs(customer.total - customer.income) + min([customer.total, customer.income, 100])\"},\n    {expression: \"customer.square\", value: \"floor(customer.income / 7) ^ 2\"},\n    {assertion: \"customer.eligible\"",
            )
    }

    pub fn members(seed: u64) -> Batch {
        Batch::new("anchor-members", &Self::members_policy(), Self::rows(300, seed))
    }

    pub fn lists() -> Batch {
        let policy = r#"{
  models: {
    customer: {limit: "number", rate: "number", tags: "string[]", scores: "number[]", companies: "company[]", holdings: "holding[]"},
    company: {revenue: "number", active: "boolean", depts: "dept[]"},
    holding: {value: "number", lots: "lot[]"},
    dept: {size: "number", cost: "number"},
    lot: {qty: "number"},
  },
  blocks: [
    {expression: "company.cost", value: "sum(map(company.depts, #.cost))"},
    {expression: "company.wide", value: "count(company.depts, #.size > customer.limit)"},
    {expression: "company.sizes", value: "map(company.depts, #.size * 2)"},
    {expression: "customer.total", value: "sum(map(customer.companies, #.revenue))"},
    {expression: "customer.lots", value: "sum(map(customer.holdings, sum(map(#.lots, #.qty))))"},
    {expression: "customer.big", value: "map(filter(customer.holdings, #.value > customer.limit), #.value)"},
    {expression: "customer.score", value: "sum(customer.scores)"},
    {expression: "customer.vip", value: "'vip' in customer.tags"},
    {expression: "customer.hit", value: "contains(customer.scores, 3)"},
    {expression: "customer.scaled", value: "map(customer.scores, # * customer.rate)"},
    {expression: "customer.kept", value: "count(filter(customer.holdings, #.value > 10), len(#.lots) > 1)"},
    {expression: "customer.top", value: "max(map(customer.holdings, #.value))"},
  ],
}"#;
        let mut state = 23u64;
        let mut pick = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut values: Vec<Value> = (0..300)
            .map(|_| {
                let companies: Vec<Value> = (0..pick(4))
                    .map(|_| {
                        let depts: Vec<Value> = (0..pick(4)).map(|_| json!({"size": pick(30), "cost": pick(900) as f64 / 4.0})).collect();
                        json!({"revenue": pick(10_000), "active": pick(3) != 0, "depts": depts})
                    })
                    .collect();
                let holdings: Vec<Value> = (0..pick(4))
                    .map(|_| {
                        let lots: Vec<Value> = (0..pick(4)).map(|_| json!({"qty": pick(100)})).collect();
                        json!({"value": pick(50), "lots": lots})
                    })
                    .collect();
                let scores: Vec<Value> = (0..pick(5)).map(|_| json!(pick(8))).collect();
                let tags: Vec<Value> = (0..pick(3)).map(|_| json!(["a", "vip", "b"][pick(3) as usize])).collect();
                json!({"customer": {"limit": pick(20), "rate": pick(4) as f64 / 2.0, "tags": tags, "scores": scores, "companies": companies, "holdings": holdings}})
            })
            .collect();
        let odd = [
            json!({"customer": {"limit": 1, "rate": 1, "tags": [], "scores": [], "companies": [], "holdings": []}}),
            json!({"customer": {"limit": null, "rate": null, "tags": null, "scores": null, "companies": null, "holdings": null}}),
            json!({"customer": {"limit": 2, "rate": 0.5, "tags": ["vip"], "scores": [9223372036854775807i64, 9223372036854775807i64], "companies": [{"revenue": 9223372036854775807i64, "active": true, "depts": [{"size": 1, "cost": 9223372036854775807i64}, {"size": 2, "cost": 9223372036854775807i64}]}], "holdings": [{"value": 1, "lots": [{"qty": 9223372036854775807i64}, {"qty": 9223372036854775807i64}]}]}}),
            json!({"customer": {"limit": 3, "rate": 2, "tags": ["a"], "scores": [1.50, 0.00, 3], "companies": [{"revenue": 1, "active": null, "depts": null}], "holdings": [{"value": null, "lots": null}]}}),
            json!({"customer": {"limit": 3, "rate": 2, "tags": ["a", 1], "scores": ["x"], "companies": [{"revenue": "lots", "depts": []}], "holdings": [{"value": 4}]}}),
            json!({"customer": {"limit": 0, "rate": 0.25, "tags": [null], "scores": [null, 3], "companies": [{"revenue": 5, "active": false, "depts": [{"size": null, "cost": 0.000000001}]}], "holdings": [{"value": 30, "lots": [{"qty": 1}, {"qty": 2}]}]}}),
            json!({"customer": {"limit": 5}}),
        ];
        for (at, row) in odd.into_iter().enumerate() {
            values.insert(at * 37 + 4, row);
        }
        Batch::new("typed-lists", policy, values)
    }

    pub fn strings() -> [Batch; 2] {
        let policy = r#"{
  models: {
    customer: {name: "string", city: "string", tier: "string", amount: "number", qty: "number", flag: "boolean", tags: "string[]", items: "item[]"},
    item: {name: "string", qty: "number"},
  },
  blocks: [
    {expression: "item.label", value: "upper(item.name) + '-' + customer.tier"},
    {expression: "item.same", value: "item.name == customer.name"},
    {expression: "item.s", value: "string(item.qty * customer.amount)"},
    {expression: "customer.s", value: "string(customer.amount)"},
    {expression: "customer.tpl", value: "`${customer.name}:${customer.qty}:${customer.flag}`"},
    {expression: "customer.cat", value: "customer.name + '/' + customer.city"},
    {expression: "customer.up", value: "upper(customer.name)"},
    {expression: "customer.tr", value: "trim(customer.city)"},
    {expression: "customer.has", value: "contains(lower(customer.name), 'an')"},
    {expression: "customer.tz", value: "customer.tier ?? 'none'"},
    {expression: "customer.pick", value: "customer.qty > 4 ? customer.name : 'nobody'"},
    {expression: "customer.same", value: "customer.name == customer.city"},
    {expression: "customer.gold", value: "customer.tz in ['gold', 'none']"},
    {expression: "customer.names", value: "map(customer.items, #.name)"},
    {expression: "customer.cnt", value: "count(customer.items, #.name == customer.tier)"},
    {expression: "customer.intag", value: "customer.city in customer.tags"},
    {expression: "customer.rx", value: "matches(customer.name, '^[A-Z]')"},
    {expression: "customer.big", value: "string(customer.amount * 1000000000000)"},
    {expression: "customer.again", value: "upper(customer.tz) + '!'"},
    {table: "lvl", inputs: ["customer.qty"], outputs: ["customer.lvl"], rules: [
      ["> 7", "customer.name"],
      ["> 3", "customer.city"],
      ["", "'small'"],
    ]},
    {expression: "customer.lv", value: "customer.lvl + ':' + customer.tz"},
  ],
}"#;
        let mut state = 41u64;
        let mut pick = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let names = ["Anna", "bob", "  Cara  ", "Émile", "straße", "", "İstanbul", "日本", "Bart"];
        let cities = ["Berlin", "bonn", "Anna", " x ", ""];
        let tiers = ["gold", "silver", "Gold"];
        let mut values: Vec<Value> = (0..300)
            .map(|_| {
                let items: Vec<Value> = (0..pick(4)).map(|_| json!({"name": names[pick(9) as usize], "qty": pick(9)})).collect();
                let tags: Vec<Value> = (0..pick(3)).map(|_| json!(cities[pick(5) as usize])).collect();
                json!({"customer": {"name": names[pick(9) as usize], "city": cities[pick(5) as usize], "tier": tiers[pick(3) as usize], "amount": pick(40_000) as f64 / 8.0 - 2000.0, "qty": pick(10), "flag": pick(2) == 0, "tags": tags, "items": items}})
            })
            .collect();
        let long = "ab".repeat(600);
        let odd = [
            json!({"customer": {"name": null, "city": "Berlin", "tier": null, "amount": null, "qty": 3, "flag": null, "tags": null, "items": null}}),
            json!({"customer": {"name": long, "city": long, "tier": "gold", "amount": 9223372036854775807i64, "qty": 9, "flag": false, "tags": [], "items": []}}),
            json!({"customer": {"name": "ß", "city": "ǅ", "tier": "", "amount": -0.0, "qty": 0, "flag": true, "tags": ["ǅ"], "items": [{"name": "ß", "qty": 1}]}}),
            json!({"customer": {"name": "\u{1F600} x", "city": "\u{2003}a\u{2003}", "tier": "silver", "amount": 0.000000001, "qty": 6, "flag": false, "tags": ["\u{2003}a\u{2003}"], "items": [{"qty": 2}]}}),
            json!({"customer": {"qty": 5}}),
            json!({"customer": {"name": "Anna", "city": "Anna", "tier": "gold", "amount": 1.50, "qty": 8, "flag": true, "tags": ["Anna"], "items": [{"name": "gold", "qty": 9223372036854775807i64}]}}),
        ];
        for (at, row) in odd.into_iter().enumerate() {
            values.insert(at * 37 + 4, row);
        }
        let first = Batch::new("typed-strings", policy, values.clone());
        values.insert(12, json!({"customer": {"name": "q", "city": "q", "tier": "gold", "amount": 1, "qty": 2, "flag": true, "tags": [null], "items": [{"name": null, "qty": 2}]}}));
        values.insert(9, json!({"customer": {"name": 7, "city": true, "tier": 1.5, "amount": "x", "qty": 1, "flag": true, "tags": [1], "items": [{"name": 3, "qty": "a"}]}}));
        [first, Batch::new("typed-strings", policy, values)]
    }

    pub fn dates() -> [Batch; 2] {
        let policy = r#"{
  models: {
    customer: {start: "string", other: "string", days: "number", events: "event[]"},
    event: {on: "string", amt: "number"},
  },
  blocks: [
    {expression: "event.due", value: "d(event.on).add(customer.days, 'day')"},
    {expression: "event.late", value: "d(event.on) > d(customer.start)"},
    {expression: "event.s", value: "string(d(event.on).startOf('month'))"},
    {expression: "customer.f", value: "d(customer.start).format('%d.%m.%Y')"},
    {expression: "customer.y", value: "d(customer.start).year()"},
    {expression: "customer.ok", value: "d(customer.start).isValid()"},
    {expression: "customer.past", value: "d(customer.start) < d()"},
    {expression: "customer.same", value: "d(customer.start) == customer.other"},
    {expression: "customer.dts", value: "map(customer.events, d(#.on).endOf('day'))"},
    {expression: "customer.any", value: "some(customer.events, d(#.on).isBefore(d(customer.other)))"},
    {expression: "customer.first", value: "min(map(customer.events, d(#.on)))"},
    {expression: "customer.cnt", value: "count(customer.events, #.on > d(customer.start))"},
    {table: "era", inputs: ["customer.start"], outputs: ["customer.era"], rules: [
      ["< d('2000-01-01')", "'old'"],
      ["", "'new'"],
    ]},
  ],
}"#;
        let mut state = 43u64;
        let mut pick = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let days = ["2024-01-15", "2024-02-29", "2023-12-31 23:59:59", "2024-03-31T01:30:00Z", "1999-06-30", "2025-03-30 02:30:00", "9999-12-31", "2024/01/15", "2024-07"];
        let bad = ["nope", "", "2024-02-30"];
        let day = |pick: &mut dyn FnMut(u64) -> u64| match pick(15) {
            0 => json!(bad[pick(3) as usize]),
            _ => json!(days[pick(9) as usize]),
        };
        let mut values: Vec<Value> = (0..300)
            .map(|_| {
                let events: Vec<Value> = (0..1 + pick(4)).map(|_| json!({"on": day(&mut pick), "amt": pick(100)})).collect();
                json!({"customer": {"start": day(&mut pick), "other": day(&mut pick), "days": pick(400) as i64 - 200, "events": events}})
            })
            .collect();
        let odd = [
            json!({"customer": {"start": null, "other": null, "days": 0, "events": []}}),
            json!({"customer": {"start": "0001-01-01", "other": "9999-12-31", "days": 1000000000, "events": [{"on": "9999-12-31", "amt": 1}]}}),
            json!({"customer": {"start": "2024-02-29", "other": "2024-02-29T00:00:00Z", "days": -366, "events": [{"amt": 1}]}}),
        ];
        for (at, row) in odd.into_iter().enumerate() {
            values.insert(at * 37 + 4, row);
        }
        let first = Batch::new("typed-dates", policy, values.clone());
        values.insert(9, json!({"customer": {"start": 7, "other": true, "days": "x", "events": [{"on": 3, "amt": "a"}]}}));
        values.insert(15, json!({"customer": {"start": "2024-02-29", "other": null, "days": 1, "events": [{"on": null, "amt": 1}]}}));
        [first, Batch::new("typed-dates", policy, values)]
    }

    pub fn extremes() -> [Batch; 2] {
        let customer = |accounts: Value| json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": accounts}});
        let overflow = [
            customer(json!([{"balance": 9223372036854775807i64, "kind": "loan", "active": true}])),
            customer(json!([{"balance": 4611686018427387904i64, "kind": "savings", "active": true}, {"balance": 4611686018427387904i64, "kind": "savings", "active": true}])),
            json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 9223372036854775807i64, "accounts": [{"balance": 3, "kind": "loan", "active": true}]}}),
        ];
        let odd = [
            json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": -0.0, "accounts": [{"balance": 3, "kind": "loan", "active": true}]}}),
            customer(json!([])),
            customer(json!([{"balance": 0, "kind": "loan", "active": true}, {"balance": 0, "kind": "savings", "active": false}])),
            customer(json!([{"balance": 12.5, "kind": "checking", "active": true}, {"balance": 0.000000001, "kind": "savings", "active": true}])),
            customer(json!([{"balance": 0.00, "kind": "savings", "active": true}, {"balance": 1.50, "kind": "loan", "active": true}])),
            customer(json!([{"balance": -250, "kind": "other", "active": true}, {"balance": 7, "kind": "", "active": false}])),
            json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 0.25, "accounts": [{"balance": 100, "kind": "savings", "active": true}]}}),
            json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2}}),
            json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": null}}),
            json!({"customer": {"age": null, "income": 4000, "tier": "gold", "rate": 2, "accounts": []}}),
            json!({"customer": {"age": 30, "income": null, "tier": null, "rate": null, "accounts": []}}),
            json!({"customer": {"age": 17, "income": 0, "tier": "platinum", "rate": 0, "accounts": [{"balance": 5000, "kind": "loan", "active": true}]}}),
            json!({"customer": {"age": 18, "income": 0.01, "tier": "silver", "rate": 1.000, "accounts": [{"balance": 1, "kind": "checking", "active": false}]}}),
            json!({"customer": {"age": 64, "income": 1.10, "tier": "bronze", "rate": 0.00, "accounts": [{"balance": 0.10, "kind": "loan", "active": true}, {"balance": 3.333, "kind": "checking", "active": true}]}}),
        ];
        let nulls = [
            customer(json!([{"balance": -0.0, "kind": "loan", "active": true}])),
            customer(json!([{"balance": null, "kind": null, "active": null}])),
            customer(json!([{"balance": 5, "kind": "loan"}, {"kind": "savings", "active": true}])),
            customer(json!([null])),
        ];
        let policy = Self::POLICY;
        let mut values = Self::rows(128, 13);
        for (at, row) in odd.into_iter().enumerate() {
            values.insert(at * 7 + 2, row);
        }
        for (at, row) in overflow.iter().enumerate() {
            values.insert(at * 31 + 5, row.clone());
        }
        let first = Batch::new("anchor", policy, values.clone());
        for (at, row) in nulls.into_iter().enumerate() {
            values.insert(at * 11 + 3, row);
        }
        [first, Batch::new("anchor", policy, values)]
    }

    pub fn batches() -> Vec<Batch> {
        let mut out = vec![Self::base(), Self::fallback()];
        out.extend([5, 6, 7].map(Self::fusion));
        out.extend([5, 9].map(Self::members));
        out.push(Self::lists());
        out.extend(Self::strings());
        out.extend(Self::dates());
        out.extend(Self::extremes());
        out
    }
}

#[derive(Deserialize)]
struct EvaluationFile {
    test: Vec<EvaluationCase>,
}

#[derive(Deserialize)]
struct EvaluationCase {
    name: String,
    policies: Vec<String>,
    input: toml::Value,
}

pub struct PolicyCase {
    pub name: String,
    pub path: String,
    pub workspace: PolicyWorkspace,
    pub inputs: Vec<Value>,
}

impl PolicyCase {
    const FIXTURES_DIR: &'static str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/fixtures/");
    const EVALUATION: &'static str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/evaluation.toml");

    pub fn load_all() -> Vec<PolicyCase> {
        let raw = std::fs::read_to_string(Self::EVALUATION).expect("evaluation.toml");
        let file: EvaluationFile = toml::from_str(&raw).expect("evaluation cases");
        file.test
            .into_iter()
            .map(|case| {
                let mut workspace = PolicyWorkspace::new();
                for path in &case.policies {
                    let text = std::fs::read_to_string(format!("{}{path}", Self::FIXTURES_DIR)).expect("fixture");
                    workspace.set_policy(path.as_str(), serde_json::from_str(&text).expect("policy document"));
                }
                let input: Value = serde_json::to_value(&case.input).expect("input");
                PolicyCase {
                    name: case.name,
                    path: case.policies[0].clone(),
                    workspace,
                    inputs: Self::variants(&input),
                }
            })
            .collect()
    }

    pub fn variants(base: &Value) -> Vec<Value> {
        let mut out = vec![base.clone()];
        let Some(object) = base.as_object() else {
            return out;
        };
        for (key, value) in object {
            let mut with = |replacement: Option<Value>| {
                let mut copy = object.clone();
                match replacement {
                    Some(v) => copy.insert(key.clone(), v),
                    None => copy.remove(key),
                };
                out.push(Value::Object(copy));
            };
            with(None);
            with(Some(Value::Null));
            if let Value::Object(inner) = value {
                for (field, v) in inner {
                    let mut nested = inner.clone();
                    nested.insert(
                        field.clone(),
                        match v {
                            Value::Number(_) => json!("text"),
                            Value::String(_) => json!(7),
                            Value::Bool(b) => json!(!b),
                            Value::Array(_) => json!([]),
                            _ => Value::Null,
                        },
                    );
                    with(Some(Value::Object(nested)));
                }
            }
        }
        out
    }

    pub fn leaves(value: &Value, prefix: &str, out: &mut Vec<String>) {
        let Value::Object(map) = value else {
            out.push(prefix.to_string());
            return;
        };
        for (key, child) in map {
            let path = match prefix.is_empty() {
                true => key.clone(),
                false => format!("{prefix}.{key}"),
            };
            Self::leaves(child, &path, out);
        }
    }

    pub fn computed(output: &Value, input: &Value) -> Vec<String> {
        let (mut produced, mut given) = (Vec::new(), Vec::new());
        Self::leaves(output, "", &mut produced);
        Self::leaves(input, "", &mut given);
        produced.into_iter().filter(|p| !given.contains(p)).collect()
    }

    pub fn goals(computed: &[String], i: usize) -> Vec<String> {
        let choices: Vec<Vec<String>> = std::iter::once(Vec::new())
            .chain(computed.iter().map(|c| vec![c.clone()]))
            .chain(computed.windows(2).map(|w| vec![w[0].clone(), w[1].clone()]))
            .collect();
        choices[(i * 7) % choices.len()].clone()
    }
}

pub struct Generated;

impl Generated {
    pub const PATH: &'static str = "generated.json";

    pub fn seeds() -> u64 {
        std::env::var("POLICY_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(300)
    }

    pub fn workspace(seed: u64) -> Option<PolicyWorkspace> {
        let parsed = serde_json::from_value(Gen::document(seed)).ok()?;
        let mut ws = PolicyWorkspace::new();
        ws.set_policy(Self::PATH, parsed);
        Some(ws)
    }

    pub fn inputs(seed: u64) -> Vec<Value> {
        let mut rng = Rng(seed.wrapping_mul(7919) | 1);
        (0..24)
            .map(|i| match i % 3 {
                0 => Gen::input(&mut rng),
                _ => Gen::valid_input(&mut rng),
            })
            .collect()
    }

    pub fn fingerprint(seed: u64) -> String {
        Canon::hash(&serde_json::to_string(&Canon::value(Gen::document(seed))).unwrap_or_default())
    }

    pub fn label(seed: u64) -> String {
        format!("seed {seed}")
    }
}
