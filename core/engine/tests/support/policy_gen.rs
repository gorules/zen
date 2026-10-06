use serde_json::{json, Value};

pub struct Rng(pub u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Num,
    Bool,
    Str,
}

const NUM_KEYS: &[&str] = &["doubled", "fee", "score", "bonus"];
const BOOL_KEYS: &[&str] = &["rich", "flag", "eligible"];
const STR_KEYS: &[&str] = &["label", "bucket"];

const NUM_VALUES: &[&str] = &[
    "customer.income * 2",
    "(customer.age ?? 0) + 1",
    "(customer.income + 1) / 2",
    "customer.income > 100 ? 1 : 2",
    "len(customer.tags ?? [])",
    "customer.income - 3",
    "1.5",
    "customer.income",
];
const BOOL_VALUES: &[&str] = &[
    "customer.income > 1000",
    "customer.vip == true",
    "customer.vip != true",
    "customer.tier == 'gold'",
    "customer.tier in ['gold', 'silver']",
    "customer.tier not in ['gold']",
    "customer.region != 'eu'",
    "(customer.age ?? 0) > 30",
    "customer.income in [1..500]",
    "customer.tier != 'silver'",
    "customer.region == 'us'",
];
const STR_VALUES: &[&str] = &[
    "customer.tier ?? 'none'",
    "customer.income > 5 ? 'hi' : 'lo'",
    "customer.region ?? 'zz'",
    "'x'",
];
const DT_NUM_CELLS: &[&str] = &["> 100", "<= 50", "[1..200]", "!= 2", "", ""];
const DT_TIER_CELLS: &[&str] = &["'gold'", "'gold', 'silver'", "", "'bronze'"];
const DT_BOOL_CELLS: &[&str] = &["true", "false", ""];
const DT_TAG_CELLS: &[&str] = &[
    "contains($ ?? [], 'gold')",
    "some($ ?? [], # == 'silver')",
    "all($ ?? [], # != 'x')",
    "len($ ?? []) > 0",
    "",
    "",
];
const DT_EXPR_CELLS: &[&str] = &[
    "some(customer.tags ?? [], # == 'vip')",
    "contains(customer.region ?? '', 'u')",
    "(customer.tier ?? 'none') == 'gold'",
    "len(customer.region ?? '') > 1",
    "",
];
const DT_FEE_CELLS: &[&str] = &["10", "25", "customer.income * 0.1", "", "0"];
const DT_PERK_CELLS: &[&str] = &["'lounge'", "'priority'", "", "'starter'"];
const DT_KIND_CELLS: &[&str] = &["'savings'", "'savings', 'checking'", "", "'other'"];

const ACC_NUM_KEYS: &[&str] = &["doubled", "fee", "weight"];
const ACC_BOOL_KEYS: &[&str] = &["ok", "flagged"];
const ACC_STR_KEYS: &[&str] = &["label"];
const ACC_NUM_VALUES: &[&str] = &[
    "account.balance * 2",
    "(account.balance ?? 0) + customer.income",
    "account.customer.income + 1",
    "len(customer.accounts ?? [])",
    "account.balance - customer.age",
    "1.5",
];
const ACC_BOOL_VALUES: &[&str] = &[
    "account.active == true",
    "account.balance > 100",
    "account.kind == 'savings'",
    "account.kind in ['savings', 'checking']",
    "(account.balance ?? 0) > customer.income",
    "account.customer.vip == true",
    "account.active != true",
];
const ACC_STR_VALUES: &[&str] = &[
    "account.kind ?? 'none'",
    "account.balance > 5 ? 'hi' : 'lo'",
    "'x'",
];
const COLLECTION_VALUES: &[&str] = &[
    "sum(map(customer.accounts ?? [], #.balance ?? 0))",
    "len(customer.accounts ?? [])",
    "sum(map(customer.picks ?? [], #.price))",
    "len(customer.picks ?? [])",
];

pub struct Gen {
    rng: Rng,
    blocks: Vec<Value>,
    written: Vec<(String, Kind)>,
    written_acc: Vec<(String, Kind)>,
    product_block: bool,
    counter: usize,
}

impl Gen {
    fn id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn free_key(&mut self, kind: Kind) -> Option<String> {
        let pool: &[&str] = match kind {
            Kind::Num => NUM_KEYS,
            Kind::Bool => BOOL_KEYS,
            Kind::Str => STR_KEYS,
        };
        let candidates: Vec<&str> = pool
            .iter()
            .copied()
            .filter(|key| !self.written.iter().any(|(seen, _)| seen == key))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        Some((*self.rng.pick(&candidates)).to_string())
    }

    fn free_acc_key(&mut self, kind: Kind) -> Option<String> {
        let pool: &[&str] = match kind {
            Kind::Num => ACC_NUM_KEYS,
            Kind::Bool => ACC_BOOL_KEYS,
            Kind::Str => ACC_STR_KEYS,
        };
        let candidates: Vec<&str> = pool
            .iter()
            .copied()
            .filter(|key| !self.written_acc.iter().any(|(seen, _)| seen == key))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        Some((*self.rng.pick(&candidates)).to_string())
    }

    fn acc_value(&mut self, kind: Kind) -> String {
        let prior: Vec<String> = self
            .written_acc
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(key, _)| format!("account.{key}"))
            .collect();
        if !prior.is_empty() && self.rng.chance(35) {
            let key = self.rng.pick(&prior).clone();
            return match kind {
                Kind::Num => format!("{key} + 1"),
                Kind::Bool => format!("not {key}"),
                Kind::Str => key,
            };
        }
        let pool: &[&str] = match kind {
            Kind::Num => ACC_NUM_VALUES,
            Kind::Bool => ACC_BOOL_VALUES,
            Kind::Str => ACC_STR_VALUES,
        };
        (*self.rng.pick(pool)).to_string()
    }

    fn account_expression_block(&mut self) {
        let kind = *self.rng.pick(&[Kind::Num, Kind::Bool, Kind::Str]);
        let Some(key) = self.free_acc_key(kind) else {
            return;
        };
        let value = self.acc_value(kind);
        self.block(
            "expression",
            json!({ "key": format!("account.{key}"), "value": value }),
        );
        self.written_acc.push((key, kind));
    }

    fn account_match_block(&mut self) {
        let kind = *self.rng.pick(&[Kind::Num, Kind::Str]);
        let Some(key) = self.free_acc_key(kind) else {
            return;
        };
        let arms = 1 + self.rng.below(3);
        let mut list: Vec<Value> = (0..arms)
            .map(|i| {
                let condition = self.acc_value(Kind::Bool);
                let value = self.acc_value(kind);
                json!({ "id": format!("a{i}"), "condition": condition, "value": value })
            })
            .collect();
        let default = self.acc_value(kind);
        list.push(json!({ "id": "default", "condition": "", "value": default }));
        self.block(
            "match",
            json!({ "key": format!("account.{key}"), "arms": list }),
        );
        self.written_acc.push((key, kind));
    }

    fn account_assertion_block(&mut self) {
        let Some(key) = self.free_acc_key(Kind::Bool) else {
            return;
        };
        let count = 1 + self.rng.below(3);
        let conditions: Vec<Value> = (0..count)
            .map(|i| {
                let expression = self.acc_value(Kind::Bool);
                let operator = if self.rng.chance(50) { "and" } else { "or" };
                json!({ "id": format!("c{i}"), "expression": expression, "operator": operator, "depth": 0 })
            })
            .collect();
        self.block(
            "assertion",
            json!({ "output": format!("account.{key}"), "conditions": conditions }),
        );
        self.written_acc.push((key, Kind::Bool));
    }

    fn account_table_block(&mut self) {
        let Some(key) = self.free_acc_key(Kind::Num) else {
            return;
        };
        let columns: Vec<(&str, &[&str])> = vec![
            ("account.balance", DT_NUM_CELLS),
            ("account.kind", DT_KIND_CELLS),
            ("customer.income", DT_NUM_CELLS),
        ];
        let n_in = 1 + self.rng.below(2);
        let chosen: Vec<(&str, &[&str])> = (0..n_in).map(|_| *self.rng.pick(&columns)).collect();
        let inputs: Vec<Value> = chosen
            .iter()
            .enumerate()
            .map(|(i, (field, _))| json!({ "id": format!("i{i}"), "field": field, "name": format!("i{i}") }))
            .collect();
        let outputs = vec![json!({ "id": "o1", "field": format!("account.{key}"), "name": "fee" })];
        let n_rules = 1 + self.rng.below(4);
        let rules: Vec<Value> = (0..n_rules)
            .map(|r| {
                let mut rule = json!({ "_id": format!("r{r}") });
                for (i, (_, cells)) in chosen.iter().enumerate() {
                    rule[format!("i{i}")] = json!(*self.rng.pick(cells));
                }
                rule["o1"] = json!(*self
                    .rng
                    .pick(&["10", "25", "account.balance * 0.1", "", "0"]));
                rule
            })
            .collect();
        self.block(
            "decisionTable",
            json!({ "hitPolicy": "first", "inputs": inputs, "outputs": outputs, "rules": rules }),
        );
        self.written_acc.push((key, Kind::Num));
    }

    fn collection_block(&mut self) {
        let Some(key) = self.free_key(Kind::Num) else {
            return;
        };
        let value = if self
            .written_acc
            .iter()
            .any(|(k, kind)| k == "doubled" && *kind == Kind::Num)
            && self.rng.chance(50)
        {
            "sum(map(customer.accounts ?? [], #.doubled ?? 0))".to_string()
        } else {
            (*self.rng.pick(COLLECTION_VALUES)).to_string()
        };
        self.block(
            "expression",
            json!({ "key": format!("customer.{key}"), "value": value }),
        );
        self.written.push((key, Kind::Num));
    }

    fn product_block(&mut self) {
        if self.product_block {
            return;
        }
        self.product_block = true;
        self.block(
            "expression",
            json!({ "key": "product.discounted", "value": "product.price * 0.9" }),
        );
    }

    fn written_of(&self, kind: Kind) -> Vec<String> {
        self.written
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(key, _)| format!("customer.{key}"))
            .collect()
    }

    fn value(&mut self, kind: Kind) -> String {
        let prior = self.written_of(kind);
        if !prior.is_empty() && self.rng.chance(35) {
            let key = self.rng.pick(&prior).clone();
            return match kind {
                Kind::Num => match self.rng.below(6) {
                    0 => format!("{key} + 1"),
                    1 => format!("{key} * 2"),
                    2 => format!("round({key} / 3, 2)"),
                    3 => format!("max([{key}, 10])"),
                    4 => format!("min([{key} * 0.5, 100, customer.income])"),
                    _ => format!("abs({key} - 5) + floor({key})"),
                },
                Kind::Bool => {
                    if self.rng.chance(50) {
                        key
                    } else {
                        format!("not {key}")
                    }
                }
                Kind::Str => key,
            };
        }
        let nums = self.written_of(Kind::Num);
        if kind == Kind::Bool && !nums.is_empty() && self.rng.chance(25) {
            let key = self.rng.pick(&nums).clone();
            return format!("{key} > 10");
        }
        let pool: &[&str] = match kind {
            Kind::Num => NUM_VALUES,
            Kind::Bool => BOOL_VALUES,
            Kind::Str => STR_VALUES,
        };
        (*self.rng.pick(pool)).to_string()
    }

    fn block(&mut self, kind: &str, data: Value) {
        let id = self.id(kind);
        self.blocks
            .push(json!({ "id": id, "type": kind, "props": { "data": data } }));
    }

    fn expression_block(&mut self) {
        let kind = *self.rng.pick(&[Kind::Num, Kind::Bool, Kind::Str]);
        let Some(key) = self.free_key(kind) else {
            return;
        };
        let value = self.value(kind);
        self.block(
            "expression",
            json!({ "key": format!("customer.{key}"), "value": value }),
        );
        self.written.push((key, kind));
    }

    fn assertion_block(&mut self) {
        let Some(key) = self.free_key(Kind::Bool) else {
            return;
        };
        let count = 1 + self.rng.below(3);
        let conditions: Vec<Value> = (0..count)
            .map(|i| {
                let expression = self.value(Kind::Bool);
                let operator = if self.rng.chance(50) { "and" } else { "or" };
                let depth = if i == 1 && count == 3 && self.rng.chance(50) { 1 } else { 0 };
                json!({ "id": format!("c{i}"), "expression": expression, "operator": operator, "depth": depth })
            })
            .collect();
        self.block(
            "assertion",
            json!({ "output": format!("customer.{key}"), "conditions": conditions }),
        );
        self.written.push((key, Kind::Bool));
    }

    fn match_block(&mut self) {
        let kind = *self.rng.pick(&[Kind::Num, Kind::Str]);
        let Some(key) = self.free_key(kind) else {
            return;
        };
        let arms = 1 + self.rng.below(3);
        let mut list: Vec<Value> = (0..arms)
            .map(|i| {
                let condition = self.value(Kind::Bool);
                let value = self.value(kind);
                json!({ "id": format!("a{i}"), "condition": condition, "value": value })
            })
            .collect();
        let default = self.value(kind);
        list.push(json!({ "id": "default", "condition": "", "value": default }));
        self.block(
            "match",
            json!({ "key": format!("customer.{key}"), "arms": list }),
        );
        self.written.push((key, kind));
    }

    fn table_block(&mut self) {
        let Some(key) = self.free_key(Kind::Num) else {
            return;
        };
        let with_perks =
            self.rng.chance(50) && !self.written.iter().any(|(seen, _)| seen == "perks");
        let columns: Vec<(&str, &[&str])> = vec![
            ("customer.income", DT_NUM_CELLS),
            ("customer.tier", DT_TIER_CELLS),
            ("customer.vip", DT_BOOL_CELLS),
            ("customer.tags", DT_TAG_CELLS),
            ("", DT_EXPR_CELLS),
        ];
        let n_in = 1 + self.rng.below(2);
        let chosen: Vec<(&str, &[&str])> = (0..n_in).map(|_| *self.rng.pick(&columns)).collect();
        let inputs: Vec<Value> = chosen
            .iter()
            .enumerate()
            .map(|(i, (field, _))| json!({ "id": format!("i{i}"), "field": field, "name": format!("i{i}") }))
            .collect();
        let mut outputs =
            vec![json!({ "id": "o1", "field": format!("customer.{key}"), "name": "fee" })];
        if with_perks {
            outputs.push(json!({ "id": "o2", "field": "customer.perks[]", "name": "perks" }));
        }
        let n_rules = 1 + self.rng.below(4);
        let rules: Vec<Value> = (0..n_rules)
            .map(|r| {
                let mut rule = json!({ "_id": format!("r{r}") });
                for (i, (_, cells)) in chosen.iter().enumerate() {
                    rule[format!("i{i}")] = json!(*self.rng.pick(cells));
                }
                rule["o1"] = json!(*self.rng.pick(DT_FEE_CELLS));
                if with_perks {
                    rule["o2"] = json!(*self.rng.pick(DT_PERK_CELLS));
                }
                rule
            })
            .collect();
        self.block(
            "decisionTable",
            json!({ "hitPolicy": "first", "inputs": inputs, "outputs": outputs, "rules": rules }),
        );
        self.written.push((key, Kind::Num));
        if with_perks {
            self.written.push(("perks".to_string(), Kind::Str));
        }
    }

    pub fn document(seed: u64) -> Value {
        let mut builder = Gen {
            rng: Rng(seed | 1),
            blocks: Vec::new(),
            written: Vec::new(),
            written_acc: Vec::new(),
            product_block: false,
            counter: 0,
        };
        builder.block(
            "dataModel",
            json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "income", "type": "number", "array": false, "optional": false },
                    { "id": "p2", "name": "vip", "type": "boolean", "array": false, "optional": true },
                    { "id": "p3", "name": "region", "type": "string", "array": false, "optional": true },
                    { "id": "p4", "name": "tier", "type": "string", "enum": ["gold", "silver", "bronze"], "array": false, "optional": true },
                    { "id": "p5", "name": "age", "type": "number", "array": false, "optional": true },
                    { "id": "p6", "name": "tags", "type": "string", "array": true, "optional": true },
                    { "id": "p7", "name": "accounts", "type": "relationship", "target": "account", "array": true, "optional": true },
                    { "id": "p8", "name": "picks", "type": "reference", "target": "product", "array": true, "optional": true }
                ]
            }),
        );
        builder.block(
            "dataModel",
            json!({
                "name": "account",
                "properties": [
                    { "id": "a1", "name": "balance", "type": "number", "array": false, "optional": true },
                    { "id": "a2", "name": "kind", "type": "string", "enum": ["savings", "checking", "other"], "array": false, "optional": true },
                    { "id": "a3", "name": "active", "type": "boolean", "array": false, "optional": true }
                ]
            }),
        );
        builder.block(
            "dataModel",
            json!({
                "name": "product",
                "properties": [
                    { "id": "d1", "name": "id", "type": "string", "array": false, "optional": false },
                    { "id": "d2", "name": "price", "type": "number", "array": false, "optional": false }
                ]
            }),
        );
        let count = 2 + builder.rng.below(6);
        for _ in 0..count {
            match builder.rng.below(10) {
                0 => builder.expression_block(),
                1 => builder.assertion_block(),
                2 => builder.match_block(),
                3 => builder.table_block(),
                4 => builder.account_expression_block(),
                5 => builder.account_match_block(),
                6 => builder.account_assertion_block(),
                7 => builder.account_table_block(),
                8 => builder.collection_block(),
                _ => builder.product_block(),
            }
        }
        json!({ "blocks": builder.blocks })
    }

    pub fn input(rng: &mut Rng) -> Value {
        let mut customer = serde_json::Map::new();
        if rng.chance(90) {
            let income = match rng.below(6) {
                0 => json!(null),
                1 => json!("oops"),
                2 => json!(2.5),
                _ => json!(rng.below(2000) as i64),
            };
            customer.insert("income".into(), income);
        }
        if rng.chance(60) {
            let vip = match rng.below(3) {
                0 => json!(null),
                _ => json!(rng.chance(50)),
            };
            customer.insert("vip".into(), vip);
        }
        if rng.chance(60) {
            customer.insert("region".into(), json!(*rng.pick(&["eu", "us", "apac"])));
        }
        if rng.chance(70) {
            customer.insert(
                "tier".into(),
                json!(*rng.pick(&["gold", "silver", "bronze", "platinum"])),
            );
        }
        if rng.chance(50) {
            customer.insert("age".into(), json!(rng.below(80) as i64));
        }
        if rng.chance(40) {
            customer.insert("tags".into(), json!(["a", "b"]));
        }
        if rng.chance(20) {
            customer.insert("extra".into(), json!(null));
        }
        if rng.chance(75) {
            let accounts = match rng.below(10) {
                0 => json!("x"),
                1 => json!(null),
                _ => {
                    let n = rng.below(4);
                    Value::Array((0..n).map(|_| Self::account(rng)).collect())
                }
            };
            customer.insert("accounts".into(), accounts);
        }
        let mut root = serde_json::Map::new();
        if rng.chance(35) {
            let picks = match rng.below(5) {
                0 => json!(["p1", "p9"]),
                1 => json!([]),
                2 => json!(["p2"]),
                _ => json!(["p1", "p2"]),
            };
            customer.insert("picks".into(), picks);
            if rng.chance(85) {
                root.insert(
                    "product".into(),
                    json!([{ "id": "p1", "price": 3 }, { "id": "p2", "price": 5 }]),
                );
            }
        }
        if rng.chance(15) {
            root.insert("account".into(), json!({ "balance": 1, "kind": "savings" }));
        }
        root.insert("customer".into(), Value::Object(customer));
        Value::Object(root)
    }

    pub fn valid_input(rng: &mut Rng) -> Value {
        let accounts: Vec<Value> = (0..rng.below(5))
            .map(|_| {
                let mut account = serde_json::Map::new();
                account.insert("balance".into(), json!(rng.below(500) as i64));
                account.insert(
                    "kind".into(),
                    json!(*rng.pick(&["savings", "checking", "other"])),
                );
                if rng.chance(50) {
                    account.insert("active".into(), json!(rng.chance(50)));
                }
                Value::Object(account)
            })
            .collect();
        let mut customer = serde_json::Map::new();
        customer.insert("income".into(), json!(rng.below(3000) as i64));
        customer.insert("vip".into(), json!(rng.chance(50)));
        customer.insert("region".into(), json!(*rng.pick(&["eu", "us", "apac"])));
        customer.insert("tier".into(), json!(*rng.pick(&["gold", "silver", "bronze"])));
        customer.insert("age".into(), json!(rng.below(90) as i64));
        if rng.chance(50) {
            customer.insert("tags".into(), json!(["a", "b"]));
        }
        customer.insert("accounts".into(), Value::Array(accounts));
        customer.insert(
            "picks".into(),
            json!(*rng.pick(&[&["p1", "p2"][..], &["p2"][..], &[][..]])),
        );
        let mut root = serde_json::Map::new();
        root.insert("customer".into(), Value::Object(customer));
        root.insert(
            "product".into(),
            json!([{ "id": "p1", "price": 3 }, { "id": "p2", "price": 5 }]),
        );
        Value::Object(root)
    }

    fn account(rng: &mut Rng) -> Value {
        match rng.below(12) {
            0 => json!("str"),
            1 => json!(null),
            _ => {
                let mut account = serde_json::Map::new();
                if rng.chance(85) {
                    let balance = match rng.below(8) {
                        0 => json!(null),
                        1 => json!("oops"),
                        _ => json!(rng.below(500) as i64),
                    };
                    account.insert("balance".into(), balance);
                }
                if rng.chance(70) {
                    account.insert(
                        "kind".into(),
                        json!(*rng.pick(&["savings", "checking", "other"])),
                    );
                }
                if rng.chance(50) {
                    account.insert("active".into(), json!(rng.chance(50)));
                }
                if rng.chance(15) {
                    account.insert("customer".into(), json!({ "income": 1, "vip": true }));
                }
                if rng.chance(10) {
                    account.insert("doubled".into(), json!(7));
                }
                Value::Object(account)
            }
        }
    }
}
