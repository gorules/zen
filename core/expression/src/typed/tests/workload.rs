use crate::lane::Fusion;
use crate::typed::tests::data::{ColSpec, Enc, ListSpec, Mock, Row, Spec, Value};
use crate::typed::{Class, Dec, Hir, Hit, Lowering, Resolve, TableRule, TableSpec, Unsupported};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Policy,
    Graph,
}

#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub(crate) cells: Vec<String>,
    pub(crate) values: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum Block {
    Expr { key: String, source: String },
    Table { inputs: Vec<String>, outputs: Vec<String>, rules: Vec<Rule> },
}

#[derive(Clone, Debug)]
pub(crate) struct Workload {
    pub(crate) name: &'static str,
    pub(crate) spec: Spec,
    pub(crate) dialect: Dialect,
    pub(crate) member: Option<usize>,
    pub(crate) blocks: Vec<Block>,
}

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub(crate) fn pick(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

struct AnchorCustomer {
    age: u64,
    income: u64,
    tier: &'static str,
    rate: u64,
    accounts: Vec<(u64, &'static str, bool)>,
}

impl Block {
    pub(crate) fn lower(&self, lowering: &mut Lowering, resolve: &mut dyn Resolve, hit: Hit) -> Result<(), Unsupported> {
        match self {
            Block::Expr { key, source } => lowering.unit(&[Fusion::Output { key: key.clone(), source: source.clone() }], resolve),
            Block::Table { inputs, outputs, rules } => {
                let spec = TableSpec {
                    inputs: inputs.iter().cloned().map(Some).collect(),
                    outputs: outputs.clone(),
                    rules: rules
                        .iter()
                        .map(|rule| TableRule {
                            cells: rule.cells.iter().map(|cell| (!cell.trim().is_empty()).then(|| cell.clone())).collect(),
                            values: rule.values.iter().map(|value| Some(value.clone())).collect(),
                        })
                        .collect(),
                    hit,
                    selector: false,
                };
                lowering.table(&spec, resolve)
            }
        }
    }
}

impl Workload {
    pub(crate) const NAMES: [&'static str; 13] = ["W1", "W2", "W3", "W4", "W5", "W6", "W7", "W8", "W9", "W10", "W11", "W12", "W13"];

    pub(crate) const FAULTY: [&'static str; 7] = ["W6", "W8", "W9", "W10", "W11", "W12", "W13"];

    const W11_KEYS: [(&'static str, &'static str); 41] = [
        ("s_amt", "string(u.amt)"),
        ("s_qty", "string(u.qty)"),
        ("s_flag", "string(u.flag)"),
        ("s_tier", "string(u.tier)"),
        ("s_big", "string(u.big * 1000000000000)"),
        ("s_div", "string(u.amt / 3)"),
        ("tpl", "`${u.name}-${u.qty}:${u.amt} ${u.flag}`"),
        ("cat", "u.name + '/' + u.city"),
        ("up", "upper(u.name)"),
        ("lo", "lower(u.city)"),
        ("tr", "trim(u.name)"),
        ("has", "contains(lower(u.name), 'an')"),
        ("sw", "startsWith(upper(u.city), 'B') or endsWith($.tr, 'e')"),
        ("ln", "len(trim(u.name)) + len($.cat)"),
        ("tierz", "u.tier ?? 'none'"),
        ("pick", "u.qty > 4 ? u.name : 'nobody'"),
        ("same", "u.name == u.city"),
        ("eqlit", "upper(u.name) == 'ANNA'"),
        ("gold", "$.tierz in ['gold', 'none']"),
        ("pass", "u.city"),
        ("rx", "matches(u.name, '^[A-Z][a-z]+$')"),
        ("lbl", "u.qty > 3 ? 'big' : 'small'"),
        ("lbl2", "$.lbl + '!'"),
        ("mix", "u.qty > 5 ? $.lbl : u.tier"),
        ("lenz", "len(u.tier ?? '')"),
        ("cmpcode", "$.lbl == u.tier"),
        ("nul", "$.mix == null"),
        ("names", "map(items, #.name)"),
        ("upn", "map(items, upper(#.name) + ':' + string(#.qty))"),
        ("cnt", "count(items, #.name == u.tier)"),
        ("intag", "u.city in tags"),
        ("anyb", "some(items, contains(lower(#.tag), 'b'))"),
        ("ups", "map(tags, upper(#))"),
        ("fq", "map(filter(items, startsWith(#.name, 'A')), #.qty)"),
        ("tt", "map(items, #.tag ?? 'x')"),
        ("tc", "count(tags, # == $.pass)"),
        ("mixeq", "u.name == u.amt"),
        ("mixne", "u.tier != u.flag"),
        ("mixlit", "u.amt == 'x' or $.lbl == 3"),
        ("snull", "string(u.tier) == 'null'"),
        ("negz", "string(-u.qty) + string(-u.amt)"),
    ];

    const W12_KEYS: [(&'static str, &'static str); 6] = [
        ("item.label", "upper(item.name) + '-' + c.code"),
        ("item.same", "item.name == c.code"),
        ("item.s", "string(item.qty * c.rate)"),
        ("item.t", "`${item.name}:${item.qty}`"),
        ("item.k", "item.qty > 2 ? item.name : c.code"),
        ("item.n", "len(item.label)"),
    ];

    const W13_KEYS: [(&'static str, &'static str); 7] = [
        ("c.full", "c.code + '!'"),
        ("c.again", "lower(c.full) == 'ab!'"),
        ("c.names", "map(c.items, #.name)"),
        ("c.joined", "c.full + string(len(c.items))"),
        ("c.any", "some(c.items, #.name == c.code)"),
        ("c.lab", "map(c.items, trim(#.name) + string(c.rate))"),
        ("c.pick", "c.rate > 1 ? c.full : c.code"),
    ];

    const NAMES_W11: [&'static str; 12] = ["Anna", "bob", "  Cara  ", "Émile", "straße", "ǅemal", "", "İstanbul", "日本", "x\u{a0}", "\tTabe\n", "Bart"];

    const W8_KEYS: [(&'static str, &'static str); 24] = [
        ("total", "sum(xs)"),
        ("mean", "avg(xs)"),
        ("lo", "min(xs)"),
        ("hi", "max(filter(xs, # > 0))"),
        ("pos", "filter(xs, # > x.limit)"),
        ("doubled", "map(xs, # * 2)"),
        ("sumd", "sum($.doubled)"),
        ("big", "filter($.doubled, # > 10)"),
        ("cnt", "len(filter(xs, # >= 0))"),
        ("has", "x.n in xs"),
        ("vip", "'vip' in tags"),
        ("nvip", "'vip' not in tags"),
        ("c2", "contains(xs, 2)"),
        ("qtys", "map(filter(items, #.ok), #.qty)"),
        ("flags", "map(items, #.qty > 2)"),
        ("ct", "count(filter(items, #.ok), #.price > x.limit)"),
        ("sm", "sum(map(items ?? [], #.price * #.qty))"),
        ("lbl", "map(items, #.ok ? 'Y' : 'N')"),
        ("tc", "count(tags, # == 'a')"),
        ("nn", "len(xs)"),
        ("e", "sum(xs ?? [])"),
        ("q", "some($.qtys, # > 3)"),
        ("same", "xs ?? []"),
        ("again", "$.pos"),
    ];

    const W9_KEYS: [(&'static str, &'static str); 11] = [
        ("c.total", "sum(map(c.companies, sum(map(#.depts, #.cost))))"),
        ("c.heads", "sum(map(c.companies, sum(map(#.depts, len(#.emps)))))"),
        ("c.pay", "sum(map(c.companies, sum(map(#.depts, sum(map(filter(#.emps, #.active), #.salary))))))"),
        ("c.anyBig", "some(c.companies, some(#.depts, #.size > c.limit))"),
        ("c.cnt", "count(c.companies, #.big and len(#.depts) > 1)"),
        ("c.mx", "max(map(c.companies, #.rev))"),
        ("c.rich", "map(filter(c.companies, #.rev > c.limit), #.rev)"),
        ("c.sc", "sum(c.scores)"),
        ("c.hit", "c.limit in c.scores"),
        ("c.top", "map(c.companies, len(#.depts))"),
        ("c.deep", "count(c.companies, some(#.depts, all(#.emps, #.active)))"),
    ];

    const W10_KEYS: [(&'static str, &'static str); 8] = [
        ("co.cost", "sum(map(co.depts, #.cost))"),
        ("co.n", "len(co.depts)"),
        ("co.big", "some(co.depts, #.size > co.rev)"),
        ("co.pay", "sum(map(co.depts, sum(map(#.emps, #.salary))))"),
        ("co.share", "co.rev / (sum(map(c.companies, #.rev)) + 1)"),
        ("co.sizes", "map(co.depts, #.size * 2)"),
        ("co.hit", "c.limit in c.scores"),
        ("co.act", "count(co.depts, some(#.emps, #.active))"),
    ];

    const KINDS: [&'static str; 3] = ["savings", "checking", "loan"];
    const TIERS: [&'static str; 3] = ["gold", "silver", "bronze"];
    const REGIONS: [&'static str; 8] = ["US", "CA", "UK", "DE", "FR", "JP", "BR", "IN"];
    const COUNTRIES: [&'static str; 5] = ["US", "CA", "MX", "DE", "GB"];
    const SKUS: [&'static str; 4] = ["PRO", "STD", "BLK", "PROMO"];
    const W3_RULES: [([&'static str; 4], [&'static str; 2]); 30] = [
        ([">= 800", "> 150000", "'US', 'CA'", "true"], ["'A'", "applicant.income * 0.5"]),
        ([">= 800", "> 150000", "", "true"], ["'A'", "applicant.income * 0.45"]),
        ([">= 800", "[80000..150000]", "'US', 'CA', 'UK'", ""], ["'A'", "applicant.income * 0.4"]),
        ([">= 800", "", "", "true"], ["'B'", "applicant.income * 0.35"]),
        ([">= 800", "", "", "false"], ["'B'", "applicant.income * 0.3"]),
        (["[740..800)", "> 120000", "'US'", "true"], ["'B'", "applicant.income * 0.4"]),
        (["[740..800)", "> 120000", "'CA', 'UK', 'DE'", ""], ["'B'", "applicant.income * 0.35"]),
        (["[740..800)", "[60000..120000]", "", "true"], ["'B'", "applicant.income * 0.3 + 1000"]),
        (["[740..800)", "< 60000", "'IN', 'BR'", ""], ["'C'", "applicant.income / 4"]),
        (["[740..800)", "", "", ""], ["'C'", "applicant.income * 0.25"]),
        (["[670..740)", "> 100000", "'US', 'CA'", "true"], ["'C'", "applicant.income * 0.3"]),
        (["[670..740)", "> 100000", "", "false"], ["'C'", "applicant.income * 0.2"]),
        (["[670..740)", "[50000..100000]", "'DE', 'FR'", ""], ["'C'", "applicant.income * 0.22"]),
        (["[670..740)", "[50000..100000]", "'JP'", "true"], ["'C'", "applicant.score * 40"]),
        (["[670..740)", "< 50000", "", "true"], ["'D'", "applicant.income / 5"]),
        (["[670..740)", "", "'IN', 'BR'", ""], ["'D'", "applicant.income * 0.15"]),
        (["[580..670)", "> 90000", "", "true"], ["'D'", "applicant.income * 0.2"]),
        (["[580..670)", "> 90000", "'US'", "false"], ["'D'", "applicant.income * 0.12"]),
        (["[580..670)", "[40000..90000]", "'UK', 'DE', 'FR'", ""], ["'D'", "applicant.income * 0.1 + 500"]),
        (["[580..670)", "[40000..90000]", "", "true"], ["'D'", "applicant.score * 20"]),
        (["[580..670)", "< 40000", "'JP'", ""], ["'E'", "5000"]),
        (["[580..670)", "", "", "false"], ["'E'", "applicant.income * 0.05"]),
        (["[500..580)", "> 80000", "", "true"], ["'E'", "applicant.income * 0.08"]),
        (["[500..580)", "> 80000", "'US', 'CA'", ""], ["'E'", "applicant.income * 0.06"]),
        (["[500..580)", "", "'IN'", "true"], ["'E'", "2500"]),
        (["[500..580)", "[20000..80000]", "", ""], ["'F'", "applicant.income * 0.03"]),
        (["< 500", "> 100000", "", "true"], ["'F'", "applicant.income * 0.02"]),
        (["< 500", "", "'US', 'CA', 'UK'", "true"], ["'F'", "1000"]),
        (["< 500", "[30000..100000]", "", "false"], ["'F'", "500"]),
        (["[300..400)", "", "", ""], ["'X'", "0"]),
    ];

    const W5_KEYS: [(&'static str, &'static str); 18] = [
        ("r2", "round(m.x * 1.075, 2)"),
        ("r0", "round(m.x / 3)"),
        ("fl", "floor(m.x) + ceil(m.y)"),
        ("tr", "trunc(m.y / 7, 3)"),
        ("ab", "abs(m.x - m.y)"),
        ("md", "m.n % 7"),
        ("mx", "m.x % 0.3"),
        ("pw", "m.x ^ 2"),
        ("pw3", "m.n ^ 3 + m.y ^ -1"),
        ("lo", "min([m.x, m.y, 10])"),
        ("hi", "max([m.x, $.r2, m.n])"),
        ("av", "avg([m.x, m.y])"),
        ("sm", "sum([m.x, m.y, m.n])"),
        ("b", "bool(m.flag) and m.x > 0"),
        ("q3", "m.x / 3 > 1"),
        ("neg", "-m.x"),
        ("c", "$.q3 ? round($.av, 1) : $.lo"),
        ("k", "m.n > 2 ? m.x / 7 : (m.n < -2 ? m.x * m.y : $.pw)"),
    ];

    const W6_KEYS: [(&'static str, &'static str); 10] = [
        ("c.any", "some(c.items, #.qty > 3)"),
        ("c.all", "all(c.items, #.ok)"),
        ("c.none", "none(c.items, #.price > c.limit)"),
        ("c.one", "one(c.items, #.ok)"),
        ("c.cnt", "count(c.items, #.qty > 2)"),
        ("c.min", "min(map(c.items, #.price))"),
        ("c.max", "max(map(filter(c.items, #.ok), #.qty * #.price))"),
        ("c.avg", "avg(map(c.items, #.price))"),
        ("c.sfilt", "some(filter(c.items, #.ok), #.price / #.qty > 2)"),
        ("c.total", "sum(map(c.items, #.price * #.qty / 3))"),
    ];

    const W7_KEYS: [(&'static str, &'static str); 4] = [
        ("item.r", "round(item.price * c.limit / 100, 2)"),
        ("item.m", "item.qty % 2 == 0 ? abs(item.price) : -item.price"),
        ("item.w", "item.price / 3"),
        ("item.f", "floor(item.w * 10) ^ 2"),
    ];

    const W4_KEYS: [(&'static str, &'static str); 11] = [
        ("subtotal", "order.qty * order.price"),
        ("rate", "order.discount ?? 0"),
        ("discounted", "$.subtotal - $.subtotal * $.rate / 100"),
        ("heavy", "order.weight > 20"),
        ("shipping", "$.heavy ? 25 : (order.express ? 15 : 5)"),
        ("domestic", "order.country in ['US', 'CA']"),
        ("tax", "$.domestic ? $.discounted * 0.07 : 0"),
        ("promo", "startsWith(order.sku, 'PRO')"),
        ("total", "$.discounted + $.shipping + $.tax - ($.promo ? 5 : 0)"),
        ("vip", "$.total >= 500 and not $.promo"),
        ("skuLen", "len(order.sku)"),
    ];

    pub(crate) fn get(name: &str) -> Workload {
        match name {
            "W1" => Self::w1(),
            "W2" => Self::w2(),
            "W3" => Self::w3(),
            "W5" => Self::w5(),
            "W6" => Self::w6(),
            "W7" => Self::w7(),
            "W8" => Self::w8(),
            "W9" => Self::w9(),
            "W10" => Self::w10(),
            "W11" => Self::w11(),
            "W12" => Self::w12(),
            "W13" => Self::w13(),
            _ => Self::w4(),
        }
    }

    pub(crate) fn hir(&self) -> Result<Hir, Unsupported> {
        let member = self.member.and_then(|m| self.spec.lists.get(m)).map(|l| (l.alias, l.path));
        let mut lowering = Lowering::new(member);
        let mut mock = Mock { spec: &self.spec, member: self.member };
        for block in &self.blocks {
            let hit = match self.dialect {
                Dialect::Policy => Hit::Strict,
                Dialect::Graph => Hit::Lenient,
            };
            block.lower(&mut lowering, &mut mock, hit)?;
        }
        Ok(lowering.finish())
    }

    pub(crate) fn rows(&self, count: usize) -> Vec<Row> {
        match self.name {
            "W1" => Self::anchor(count).iter().map(Self::w1_row).collect(),
            "W2" => Self::anchor(count).iter().map(Self::w2_row).collect(),
            "W3" => Self::w3_rows(count),
            "W5" => Self::w5_rows(count),
            "W6" | "W7" => Self::w6_rows(count),
            "W8" => Self::w8_rows(count),
            "W9" | "W10" => Self::w9_rows(count),
            "W11" => Self::w11_rows(count),
            "W12" | "W13" => Self::w12_rows(count),
            _ => Self::w4_rows(count),
        }
    }

    pub(crate) fn extremes(&self) -> Vec<Row> {
        match self.name {
            "W1" => Self::w1_extremes(),
            "W2" => Self::w2_extremes(),
            "W3" => Self::w3_extremes(),
            "W5" => Self::w5_extremes(),
            "W6" | "W7" => Self::w6_extremes(),
            "W8" => Self::w8_extremes(),
            "W9" | "W10" => Self::w9_extremes(),
            "W11" => Self::w11_extremes(),
            "W12" | "W13" => Self::w12_extremes(),
            _ => Self::w4_extremes(),
        }
    }

    pub(crate) fn mixed(&self, count: usize) -> Vec<Row> {
        let mut rows = self.rows(count);
        for (at, row) in self.extremes().into_iter().enumerate() {
            let index = (at * 7 + 3).min(rows.len());
            rows.insert(index, row);
        }
        rows
    }

    fn col(path: &'static str, class: Class, enc: Enc) -> ColSpec {
        ColSpec::new(path, class, enc)
    }

    fn expr(key: &str, source: &str) -> Block {
        Block::Expr { key: key.to_string(), source: source.to_string() }
    }

    fn n(mant: i64) -> Value {
        Value::Num(Dec::int(mant))
    }

    fn d(mant: i64, scale: u8) -> Value {
        Value::Num(Dec::new(mant, scale))
    }

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    fn w1() -> Workload {
        Workload {
            name: "W1",
            spec: Spec {
                columns: vec![Self::col("customer.rate", Class::Num, Enc::Scaled)],
                lists: vec![ListSpec {
                    parent: None,
                    path: "customer.accounts",
                    alias: "account",
                    fields: vec![Self::col("balance", Class::Num, Enc::Dec), Self::col("kind", Class::Text, Enc::Dict)],
                }],
            },
            dialect: Dialect::Policy,
            member: Some(0),
            blocks: vec![
                Self::expr("account.fee", "account.balance * customer.rate / 100 * (account.kind == 'loan' ? 2 : 1)"),
                Self::expr("account.label", "account.kind == 'savings' ? 'S' : (account.kind == 'checking' ? 'C' : 'L')"),
            ],
        }
    }

    fn w2() -> Workload {
        let rules = [
            ("'gold'", "'low'", "0.15"),
            ("'gold'", "'mid'", "0.1"),
            ("'silver'", "'low'", "0.1"),
            ("'silver'", "'mid'", "0.05"),
            ("", "'low'", "0.02"),
            ("", "", "0"),
        ];
        Workload {
            name: "W2",
            spec: Spec {
                columns: vec![
                    Self::col("customer.age", Class::Num, Enc::Scaled),
                    Self::col("customer.income", Class::Num, Enc::Dec),
                    Self::col("customer.tier", Class::Text, Enc::Text),
                ],
                lists: vec![ListSpec {
                    parent: None,
                    path: "customer.accounts",
                    alias: "account",
                    fields: vec![
                        Self::col("balance", Class::Num, Enc::Scaled),
                        Self::col("active", Class::Bool, Enc::Bool),
                        Self::col("fee", Class::Num, Enc::Scaled),
                    ],
                }],
            },
            dialect: Dialect::Policy,
            member: None,
            blocks: vec![
                Self::expr("customer.total", "sum(map(filter(customer.accounts, #.active), #.balance))"),
                Self::expr("customer.fees", "sum(map(customer.accounts, #.fee))"),
                Self::expr("customer.risk", "customer.total > customer.income * 2 ? 'high' : (customer.total > customer.income ? 'mid' : 'low')"),
                Block::Table {
                    inputs: vec!["customer.tier".to_string(), "customer.risk".to_string()],
                    outputs: vec!["customer.discount".to_string()],
                    rules: rules
                        .iter()
                        .map(|(tier, risk, value)| Rule { cells: vec![tier.to_string(), risk.to_string()], values: vec![value.to_string()] })
                        .collect(),
                },
                Self::expr("customer.net", "customer.total - customer.fees - customer.discount"),
                Self::expr("customer.eligible", "customer.age >= 18 and customer.risk != 'high'"),
            ],
        }
    }

    fn w3() -> Workload {
        Workload {
            name: "W3",
            spec: Spec {
                columns: vec![
                    Self::col("applicant.score", Class::Num, Enc::I64),
                    Self::col("applicant.income", Class::Num, Enc::Scaled),
                    Self::col("applicant.region", Class::Text, Enc::Utf8),
                    Self::col("applicant.verified", Class::Bool, Enc::Bool),
                ],
                lists: Vec::new(),
            },
            dialect: Dialect::Graph,
            member: None,
            blocks: vec![Block::Table {
                inputs: ["applicant.score", "applicant.income", "applicant.region", "applicant.verified"].iter().map(|s| s.to_string()).collect(),
                outputs: vec!["band".to_string(), "limit".to_string()],
                rules: Self::W3_RULES
                    .iter()
                    .map(|(cells, values)| Rule {
                        cells: cells.iter().map(|c| c.to_string()).collect(),
                        values: values.iter().map(|v| v.to_string()).collect(),
                    })
                    .collect(),
            }],
        }
    }

    fn w4() -> Workload {
        Workload {
            name: "W4",
            spec: Spec {
                columns: vec![
                    Self::col("order.qty", Class::Num, Enc::I64),
                    Self::col("order.price", Class::Num, Enc::Scaled),
                    Self::col("order.discount", Class::Num, Enc::Dec),
                    Self::col("order.weight", Class::Num, Enc::Scaled),
                    Self::col("order.express", Class::Bool, Enc::Bool),
                    Self::col("order.country", Class::Text, Enc::Dict),
                    Self::col("order.sku", Class::Text, Enc::Text),
                ],
                lists: Vec::new(),
            },
            dialect: Dialect::Graph,
            member: None,
            blocks: Self::W4_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect(),
        }
    }

    fn w5() -> Workload {
        Workload {
            name: "W5",
            spec: Spec {
                columns: vec![
                    Self::col("m.x", Class::Num, Enc::Scaled),
                    Self::col("m.y", Class::Num, Enc::Dec),
                    Self::col("m.n", Class::Num, Enc::I64),
                    Self::col("m.flag", Class::Bool, Enc::Bool),
                ],
                lists: Vec::new(),
            },
            dialect: Dialect::Graph,
            member: None,
            blocks: Self::W5_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect(),
        }
    }

    fn w6_spec() -> Spec {
        Spec {
            columns: vec![Self::col("c.limit", Class::Num, Enc::Scaled)],
            lists: vec![ListSpec {
                parent: None,
                path: "c.items",
                alias: "item",
                fields: vec![Self::col("qty", Class::Num, Enc::I64), Self::col("price", Class::Num, Enc::Dec), Self::col("ok", Class::Bool, Enc::Bool)],
            }],
        }
    }

    fn w6() -> Workload {
        Workload { name: "W6", spec: Self::w6_spec(), dialect: Dialect::Policy, member: None, blocks: Self::W6_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect() }
    }

    fn w7() -> Workload {
        Workload { name: "W7", spec: Self::w6_spec(), dialect: Dialect::Policy, member: Some(0), blocks: Self::W7_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect() }
    }

    fn list(parent: Option<usize>, path: &'static str, alias: &'static str, fields: Vec<ColSpec>) -> ListSpec {
        ListSpec { parent, path, alias, fields }
    }

    fn w8() -> Workload {
        Workload {
            name: "W8",
            spec: Spec {
                columns: vec![Self::col("x.limit", Class::Num, Enc::Scaled), Self::col("x.n", Class::Num, Enc::I64)],
                lists: vec![
                    Self::list(None, "xs", "x", vec![Self::col("", Class::Num, Enc::Dec)]),
                    Self::list(None, "tags", "t", vec![Self::col("", Class::Text, Enc::Dict)]),
                    Self::list(
                        None,
                        "items",
                        "item",
                        vec![Self::col("qty", Class::Num, Enc::I64), Self::col("price", Class::Num, Enc::Scaled), Self::col("ok", Class::Bool, Enc::Bool)],
                    ),
                ],
            },
            dialect: Dialect::Graph,
            member: None,
            blocks: Self::W8_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect(),
        }
    }

    fn w9_spec() -> Spec {
        Spec {
            columns: vec![Self::col("c.limit", Class::Num, Enc::Scaled)],
            lists: vec![
                Self::list(
                    None,
                    "c.companies",
                    "co",
                    vec![Self::col("rev", Class::Num, Enc::Scaled), Self::col("big", Class::Bool, Enc::Bool), Self::col("depts", Class::Num, Enc::Items)],
                ),
                Self::list(
                    Some(0),
                    "depts",
                    "d",
                    vec![Self::col("size", Class::Num, Enc::I64), Self::col("cost", Class::Num, Enc::Scaled), Self::col("emps", Class::Num, Enc::Items)],
                ),
                Self::list(Some(1), "emps", "e", vec![Self::col("salary", Class::Num, Enc::Dec), Self::col("active", Class::Bool, Enc::Bool)]),
                Self::list(None, "c.scores", "s", vec![Self::col("", Class::Num, Enc::Scaled)]),
            ],
        }
    }

    fn w9() -> Workload {
        Workload { name: "W9", spec: Self::w9_spec(), dialect: Dialect::Policy, member: None, blocks: Self::W9_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect() }
    }

    fn w10() -> Workload {
        Workload { name: "W10", spec: Self::w9_spec(), dialect: Dialect::Policy, member: Some(0), blocks: Self::W10_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect() }
    }

    fn w11() -> Workload {
        Workload {
            name: "W11",
            spec: Spec {
                columns: vec![
                    Self::col("u.name", Class::Text, Enc::Text),
                    Self::col("u.city", Class::Text, Enc::Utf8),
                    Self::col("u.tier", Class::Text, Enc::Dict),
                    Self::col("u.amt", Class::Num, Enc::Scaled),
                    Self::col("u.qty", Class::Num, Enc::I64),
                    Self::col("u.big", Class::Num, Enc::Dec),
                    Self::col("u.flag", Class::Bool, Enc::Bool),
                ],
                lists: vec![
                    Self::list(
                        None,
                        "items",
                        "item",
                        vec![Self::col("name", Class::Text, Enc::Dict), Self::col("qty", Class::Num, Enc::I64), Self::col("tag", Class::Text, Enc::Text)],
                    ),
                    Self::list(None, "tags", "t", vec![Self::col("", Class::Text, Enc::Utf8)]),
                ],
            },
            dialect: Dialect::Graph,
            member: None,
            blocks: Self::W11_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect(),
        }
    }

    fn w12_spec() -> Spec {
        Spec {
            columns: vec![Self::col("c.code", Class::Text, Enc::Text), Self::col("c.rate", Class::Num, Enc::Scaled)],
            lists: vec![Self::list(None, "c.items", "item", vec![Self::col("name", Class::Text, Enc::Dict), Self::col("qty", Class::Num, Enc::I64)])],
        }
    }

    fn w12() -> Workload {
        Workload { name: "W12", spec: Self::w12_spec(), dialect: Dialect::Policy, member: Some(0), blocks: Self::W12_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect() }
    }

    fn w13() -> Workload {
        Workload { name: "W13", spec: Self::w12_spec(), dialect: Dialect::Policy, member: None, blocks: Self::W13_KEYS.iter().map(|(k, s)| Self::expr(k, s)).collect() }
    }

    fn text(rng: &mut Rng, pool: &[&str], nulls: u64) -> Value {
        match rng.pick(nulls) {
            0 => Value::Null,
            _ => Self::s(pool[rng.pick(pool.len() as u64) as usize]),
        }
    }

    fn w11_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(31);
        let tiers = ["gold", "silver", "big", "small", "Gold"];
        let cities = ["Berlin", "bonn", "Anna", "  Bart", "Émile", "x\u{a0}", ""];
        let tags = ["b", "Berlin", "Anna", "Ünï", ""];
        (0..count)
            .map(|_| {
                let items: Vec<Vec<Value>> = match rng.pick(25) {
                    0 => vec![Vec::new()],
                    _ => (0..rng.pick(5))
                        .map(|_| vec![Self::text(&mut rng, &Self::NAMES_W11, 40), Self::n(rng.pick(9) as i64), Self::text(&mut rng, &tags, 40)])
                        .collect(),
                };
                let tg: Vec<Vec<Value>> = match rng.pick(30) {
                    0 => vec![Vec::new()],
                    _ => (0..rng.pick(4)).map(|_| vec![Self::text(&mut rng, &cities, 40)]).collect(),
                };
                let flag = match rng.pick(30) {
                    0 => Value::Null,
                    k => Value::Bool(k % 2 == 0),
                };
                let amt = match rng.pick(30) {
                    0 => Value::Null,
                    _ => Self::d(rng.pick(200_001) as i64 - 100_000, rng.pick(4) as u8),
                };
                let columns = vec![
                    Self::text(&mut rng, &Self::NAMES_W11, 60),
                    Self::text(&mut rng, &cities, 60),
                    Self::text(&mut rng, &tiers, 5),
                    amt,
                    Self::n(rng.pick(10) as i64),
                    Self::d(rng.pick(2_000_001) as i64 - 1_000_000, rng.pick(3) as u8),
                    flag,
                ];
                Row { columns, lists: vec![items, tg] }
            })
            .collect()
    }

    fn w11_extremes() -> Vec<Row> {
        let row = |name: Value, city: Value, tier: Value, amt: Value, qty: Value, big: Value| Row {
            columns: vec![name, city, tier, amt, qty, big, Value::Bool(true)],
            lists: vec![vec![vec![Self::s("Ab"), Self::n(1), Value::Null]], vec![vec![Self::s("bonn")]]],
        };
        let long = "ab".repeat(700);
        vec![
            row(Self::s(""), Self::s(""), Self::s(""), Self::d(0, 3), Self::n(0), Self::d(0, 2)),
            row(Self::s(&long), Self::s(&long), Value::Null, Self::n(i64::MAX), Self::n(i64::MAX), Self::n(i64::MAX)),
            row(Self::s("ß"), Self::s("ǅ"), Self::s("gold"), Self::d(1, 28), Self::n(-1), Self::n(i64::MIN)),
            row(Self::s("\u{1F600} x"), Self::s("İ"), Self::s("big"), Self::d(-1, 28), Self::n(7), Self::d(-5, 1)),
            row(Self::s(" \u{2003}a\u{2003} "), Self::s("\u{b}v\u{c}"), Self::s("small"), Self::n(i64::MIN), Self::n(4), Self::d(150, 2)),
            row(Self::s("Anna"), Self::s("Anna"), Self::s("small"), Self::d(-1500, 3), Self::n(6), Self::d(1, 0)),
        ]
    }

    fn w12_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(37);
        let codes = ["AB", "ab", "Zoë", "", "q"];
        let names = ["ab", "AB", "q", "Zoë", "", " ab ", "ß"];
        (0..count)
            .map(|r| {
                let items: Vec<Vec<Value>> = match rng.pick(30) {
                    0 => vec![Vec::new()],
                    _ => (0..if r % 113 == 5 { 1100 } else { rng.pick(5) as usize })
                        .map(|_| vec![Self::text(&mut rng, &names, 50), Self::n(rng.pick(6) as i64)])
                        .collect(),
                };
                let rate = match rng.pick(40) {
                    0 => Value::Null,
                    _ => Self::d(rng.pick(400) as i64, rng.pick(3) as u8),
                };
                Row { columns: vec![Self::text(&mut rng, &codes, 40), rate], lists: vec![items] }
            })
            .collect()
    }

    fn w12_extremes() -> Vec<Row> {
        let row = |code: Value, rate: Value, items: Vec<[Value; 2]>| Row { columns: vec![code, rate], lists: vec![items.into_iter().map(|i| i.to_vec()).collect()] };
        vec![
            row(Self::s(""), Self::n(0), Vec::new()),
            row(Self::s("ab"), Self::d(1, 28), vec![[Self::s("ab"), Self::n(i64::MAX)], [Self::s(""), Self::n(0)]]),
            row(Value::Null, Self::n(2), vec![[Self::s("x"), Self::n(3)]]),
            row(Self::s("ǅ"), Self::n(i64::MAX), vec![[Self::s("ǅ"), Self::n(2)], [Value::Null, Self::n(1)]]),
            row(Self::s("\u{1F600}"), Self::d(-5, 1), vec![[Self::s("\u{1F600}"), Value::Null]]),
        ]
    }

    fn w8_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(23);
        let tags = ["a", "b", "vip", "c"];
        (0..count)
            .map(|r| {
                let xs: Vec<Vec<Value>> = match rng.pick(30) {
                    0 => vec![Vec::new()],
                    _ => (0..if r % 97 == 3 { 1300 } else { rng.pick(6) as usize })
                        .map(|_| match rng.pick(60) {
                            0 => vec![Value::Null],
                            _ => vec![Self::d(rng.pick(4001) as i64 - 1000, rng.pick(3) as u8)],
                        })
                        .collect(),
                };
                let tg: Vec<Vec<Value>> = (0..rng.pick(4)).map(|_| vec![Self::s(tags[rng.pick(4) as usize])]).collect();
                let items: Vec<Vec<Value>> = match rng.pick(25) {
                    0 => vec![Vec::new()],
                    _ => (0..rng.pick(5))
                        .map(|_| {
                            let ok = match rng.pick(50) {
                                0 => Value::Null,
                                k => Value::Bool(k % 3 != 0),
                            };
                            vec![Self::n(rng.pick(8) as i64), Self::d(rng.pick(50_000) as i64, 2), ok]
                        })
                        .collect(),
                };
                let limit = match rng.pick(40) {
                    0 => Value::Null,
                    _ => Self::d(rng.pick(2000) as i64, 1),
                };
                Row { columns: vec![limit, Self::n(rng.pick(20) as i64 - 10)], lists: vec![xs, tg, items] }
            })
            .collect()
    }

    fn w8_extremes() -> Vec<Row> {
        let row = |limit: Value, n: Value, xs: Vec<Value>, items: Vec<[Value; 3]>| Row {
            columns: vec![limit, n],
            lists: vec![xs.into_iter().map(|x| vec![x]).collect(), vec![vec![Self::s("vip")]], items.into_iter().map(|i| i.to_vec()).collect()],
        };
        let t = || Value::Bool(true);
        vec![
            row(Self::n(1), Self::n(2), Vec::new(), Vec::new()),
            row(Self::n(1), Value::Null, vec![Value::Null, Self::n(3)], Vec::new()),
            row(Self::n(1), Self::n(2), vec![Self::n(i64::MAX), Self::n(i64::MAX)], vec![[Self::n(i64::MAX), Self::n(3), t()]]),
            row(Self::d(5, 1), Self::n(0), vec![Self::d(1, 28), Self::d(-1, 28), Self::d(0, 3)], vec![[Self::n(2), Self::d(0, 2), t()]]),
            row(Value::Null, Self::n(2), vec![Self::n(2), Self::d(20, 1)], vec![[Self::n(1), Self::n(1), Value::Null]]),
            row(Self::n(1), Self::n(7), vec![Self::n(i64::MIN)], vec![[Self::n(9), Self::n(i64::MIN), t()]]),
            row(Self::n(1), Self::n(5), vec![Self::d(15, 1), Self::d(150, 2), Self::n(-0)], Vec::new()),
        ]
    }

    fn w9_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(29);
        (0..count)
            .map(|r| {
                let companies: Vec<Vec<Value>> = match rng.pick(40) {
                    0 => vec![Vec::new()],
                    _ => (0..if r % 131 == 7 { 300 } else { rng.pick(5) as usize })
                        .map(|_| {
                            let depts = match rng.pick(20) {
                                0 => Value::Null,
                                _ => Value::Items(
                                    (0..rng.pick(4))
                                        .map(|_| {
                                            let emps = match rng.pick(25) {
                                                0 => Value::Null,
                                                _ => Value::Items(
                                                    (0..rng.pick(4))
                                                        .map(|_| {
                                                            let active = match rng.pick(30) {
                                                                0 => Value::Null,
                                                                k => Value::Bool(k % 2 == 0),
                                                            };
                                                            vec![Self::d(rng.pick(900_000) as i64, rng.pick(3) as u8), active]
                                                        })
                                                        .collect(),
                                                ),
                                            };
                                            vec![Self::n(rng.pick(50) as i64), Self::d(rng.pick(100_000) as i64, 2), emps]
                                        })
                                        .collect(),
                                ),
                            };
                            let big = match rng.pick(30) {
                                0 => Value::Null,
                                k => Value::Bool(k % 2 == 1),
                            };
                            vec![Self::d(rng.pick(1_000_000) as i64, 1), big, depts]
                        })
                        .collect(),
                };
                let scores: Vec<Vec<Value>> = match rng.pick(30) {
                    0 => vec![Vec::new()],
                    _ => (0..rng.pick(5)).map(|_| vec![Self::d(rng.pick(300) as i64, 1)]).collect(),
                };
                Row { columns: vec![Self::d(rng.pick(400) as i64, 1)], lists: vec![companies, scores] }
            })
            .collect()
    }

    fn w9_extremes() -> Vec<Row> {
        let emp = |salary: Value, active: Value| vec![salary, active];
        let dept = |size: Value, cost: Value, emps: Value| vec![size, cost, emps];
        let company = |rev: Value, big: Value, depts: Value| vec![rev, big, depts];
        let t = || Value::Bool(true);
        let row = |limit: Value, companies: Vec<Vec<Value>>, scores: Vec<Value>| Row { columns: vec![limit], lists: vec![companies, scores.into_iter().map(|s| vec![s]).collect()] };
        vec![
            row(Self::n(1), Vec::new(), Vec::new()),
            row(Self::n(1), vec![company(Self::n(5), t(), Value::Items(Vec::new()))], vec![Self::n(1)]),
            row(Self::n(1), vec![company(Self::n(5), t(), Value::Null)], vec![Self::n(2)]),
            row(
                Self::n(1),
                vec![company(Self::n(i64::MAX), t(), Value::Items(vec![dept(Self::n(1), Self::n(i64::MAX), Value::Items(vec![emp(Self::n(i64::MAX), t()), emp(Self::n(i64::MAX), t())]))]))],
                vec![Self::n(i64::MAX), Self::n(i64::MAX)],
            ),
            row(Value::Null, vec![company(Self::n(3), Value::Null, Value::Items(vec![dept(Self::n(1), Self::n(2), Value::Null)]))], vec![Value::Null]),
            row(
                Self::d(5, 1),
                vec![company(Self::d(-1, 28), t(), Value::Items(vec![dept(Value::Null, Self::d(0, 3), Value::Items(vec![emp(Value::Null, t())]))]))],
                vec![Self::d(5, 1), Self::d(50, 2)],
            ),
            row(Self::n(-1), vec![company(Self::d(0, 2), Value::Bool(false), Value::Items(vec![dept(Self::n(1), Self::n(1), Value::Items(Vec::new()))]))], Vec::new()),
        ]
    }

    fn w5_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(17);
        (0..count)
            .map(|_| {
                let x = Self::d(rng.pick(2_000_001) as i64 - 1_000_000, rng.pick(5) as u8);
                let y = match rng.pick(25) {
                    0 => Self::d(0, rng.pick(3) as u8),
                    _ => Self::d(rng.pick(20_001) as i64 - 10_000, rng.pick(4) as u8),
                };
                let n = Self::n(rng.pick(41) as i64 - 20);
                let flag = match rng.pick(5) {
                    0 => Value::Null,
                    k => Value::Bool(k % 2 == 0),
                };
                Row { columns: vec![x, y, n, flag], lists: Vec::new() }
            })
            .collect()
    }

    fn w6_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(19);
        (0..count)
            .map(|r| {
                let len = match r % 211 {
                    5 => 1500,
                    _ => rng.pick(7) as usize,
                };
                let items = (0..len)
                    .map(|_| {
                        let qty = Self::n(rng.pick(8) as i64);
                        let price = Self::d(rng.pick(100_000) as i64, rng.pick(3) as u8);
                        let ok = match rng.pick(40) {
                            0 => Value::Null,
                            k => Value::Bool(k % 3 != 0),
                        };
                        vec![qty, price, ok]
                    })
                    .collect();
                Row { columns: vec![Self::d(rng.pick(50_000) as i64, 1)], lists: vec![items] }
            })
            .collect()
    }

    fn w5_extremes() -> Vec<Row> {
        let row = |x: Value, y: Value, n: Value, flag: Value| Row { columns: vec![x, y, n, flag], lists: Vec::new() };
        let t = || Value::Bool(true);
        vec![
            row(Self::n(i64::MAX), Self::n(1), Self::n(3), t()),
            row(Self::n(i64::MIN), Self::d(-5, 1), Self::n(-3), t()),
            row(Self::d(0, 2), Self::d(0, 3), Self::n(0), Value::Bool(false)),
            row(Value::Null, Self::n(2), Self::n(1), t()),
            row(Self::n(5), Value::Null, Self::n(1), Value::Null),
            row(Self::n(5), Self::n(2), Value::Null, t()),
            row(Self::d(1, 28), Self::d(-1, 28), Self::n(i64::MAX), t()),
            row(Self::d(-25, 1), Self::d(-15, 1), Self::n(-1), t()),
            row(Self::d(-4, 1), Self::d(5, 1), Self::n(7), t()),
            row(Self::d(i64::MAX, 9), Self::d(i64::MIN, 4), Self::n(9), t()),
            row(Self::n(10), Self::n(3), Self::n(2_000_000), t()),
        ]
    }

    fn w6_extremes() -> Vec<Row> {
        let row = |limit: Value, items: Vec<[Value; 3]>| Row { columns: vec![limit], lists: vec![items.into_iter().map(|i| i.to_vec()).collect()] };
        let t = || Value::Bool(true);
        let f = || Value::Bool(false);
        vec![
            row(Self::n(10), Vec::new()),
            row(Value::Null, vec![[Self::n(1), Self::n(5), t()]]),
            row(Self::n(10), vec![[Self::n(5), Self::n(i64::MAX), t()], [Self::n(4), Self::n(i64::MAX), t()]]),
            row(Self::n(10), vec![[Self::n(0), Self::n(7), t()], [Self::n(2), Self::n(9), f()]]),
            row(Self::n(10), vec![[Self::n(1), Value::Null, t()], [Self::n(5), Self::n(1), t()]]),
            row(Self::n(10), vec![[Self::n(5), Self::n(1), t()], [Self::n(1), Value::Null, Value::Null]]),
            row(Self::n(10), vec![[Value::Null, Self::n(1), f()], [Self::n(9), Self::n(3), t()]]),
            row(Self::d(1, 28), vec![[Self::n(3), Self::d(-1, 28), t()], [Self::n(3), Self::d(-0, 2), t()]]),
            row(Self::n(10), vec![[Self::n(1), Self::d(15, 1), t()], [Self::n(1), Self::n(15), t()], [Self::n(1), Self::d(150, 2), f()]]),
            row(Self::n(-1), vec![[Self::n(7), Self::n(i64::MIN), t()]]),
        ]
    }

    fn anchor(count: usize) -> Vec<AnchorCustomer> {
        let mut rng = Rng::new(7);
        (0..count)
            .map(|_| {
                let accounts = (0..rng.pick(8))
                    .map(|_| {
                        let balance = rng.pick(5000);
                        let kind = Self::KINDS[rng.pick(3) as usize];
                        let active = rng.pick(4) != 0;
                        (balance, kind, active)
                    })
                    .collect();
                let age = 16 + rng.pick(64);
                let income = 1000 + rng.pick(9000);
                let tier = Self::TIERS[rng.pick(3) as usize];
                let rate = 1 + rng.pick(5);
                AnchorCustomer { age, income, tier, rate, accounts }
            })
            .collect()
    }

    fn w1_row(c: &AnchorCustomer) -> Row {
        Row { columns: vec![Self::n(c.rate as i64)], lists: vec![c.accounts.iter().map(|(b, k, _)| vec![Self::n(*b as i64), Self::s(k)]).collect()] }
    }

    fn w2_row(c: &AnchorCustomer) -> Row {
        Row {
            columns: vec![Self::n(c.age as i64), Self::n(c.income as i64), Self::s(c.tier)],
            lists: vec![c
                .accounts
                .iter()
                .map(|(b, k, a)| {
                    let factor = if *k == "loan" { 2 } else { 1 };
                    vec![Self::n(*b as i64), Value::Bool(*a), Self::d((*b * c.rate * factor) as i64, 2)]
                })
                .collect()],
        }
    }

    fn w3_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(11);
        (0..count)
            .map(|_| {
                let score = 300 + rng.pick(551) as i64;
                let income = (rng.pick(200_000) * 100 + rng.pick(100)) as i64;
                let region = Self::REGIONS[rng.pick(8) as usize];
                let verified = rng.pick(3) != 0;
                Row { columns: vec![Self::n(score), Self::d(income, 2), Self::s(region), Value::Bool(verified)], lists: Vec::new() }
            })
            .collect()
    }

    fn w4_rows(count: usize) -> Vec<Row> {
        let mut rng = Rng::new(13);
        (0..count)
            .map(|_| {
                let qty = 1 + rng.pick(20) as i64;
                let price = 100 + rng.pick(50_000) as i64;
                let discount = match rng.pick(20) {
                    0 => Value::Null,
                    _ => Self::n(rng.pick(31) as i64),
                };
                let weight = 1 + rng.pick(500) as i64;
                let express = rng.pick(3) == 0;
                let country = Self::COUNTRIES[rng.pick(5) as usize];
                let sku = format!("{}-{}", Self::SKUS[rng.pick(4) as usize], rng.pick(100_000));
                Row {
                    columns: vec![Self::n(qty), Self::d(price, 2), discount, Self::d(weight, 1), Value::Bool(express), Self::s(country), Value::Str(sku)],
                    lists: Vec::new(),
                }
            })
            .collect()
    }

    fn w1_extremes() -> Vec<Row> {
        let row =
            |rate: Value, accounts: Vec<(Value, Value)>| Row { columns: vec![rate], lists: vec![accounts.into_iter().map(|(b, k)| vec![b, k]).collect()] };
        vec![
            row(Self::n(5), vec![(Self::n(9_000_000_000_000_000_000), Self::s("loan"))]),
            row(Self::d(1, 9), vec![(Self::d(125, 1), Self::s("checking")), (Self::d(1, 28), Self::s("savings"))]),
            row(Value::Null, vec![(Self::n(100), Self::s("loan"))]),
            row(Self::n(2), vec![(Value::Null, Self::s("savings")), (Self::n(3), Self::s("loan"))]),
            row(Self::n(2), vec![(Self::n(50), Value::Null)]),
            row(Self::n(3), Vec::new()),
            row(Self::n(-7), vec![(Self::n(i64::MIN), Self::s("savings"))]),
            row(Self::n(4), vec![(Self::d(i64::MAX, 2), Self::s("checking")), (Self::n(-5), Self::s("loan"))]),
            row(Self::n(1), vec![(Self::n(99_999_999_999_999_999), Self::s("loan"))]),
            row(Self::d(25, 1), vec![(Self::d(-3333, 3), Self::s("loan")), (Self::n(0), Self::s("checking"))]),
            row(Value::Null, Vec::new()),
        ]
    }

    fn w2_extremes() -> Vec<Row> {
        let row = |age: Value, income: Value, tier: Value, accounts: Vec<(Value, Value, Value)>| Row {
            columns: vec![age, income, tier],
            lists: vec![accounts.into_iter().map(|(b, a, f)| vec![b, a, f]).collect()],
        };
        let t = || Value::Bool(true);
        vec![
            row(
                Self::n(30),
                Self::n(1000),
                Self::s("gold"),
                vec![(Self::n(5_000_000_000_000_000_000), t(), Self::n(1)), (Self::n(5_000_000_000_000_000_000), t(), Self::n(1))],
            ),
            row(Self::n(30), Self::n(6_000_000_000_000_000_000), Self::s("silver"), vec![(Self::n(10), t(), Self::d(2, 1))]),
            row(Self::n(30), Self::n(4000), Self::s("gold"), vec![(Self::n(10), Value::Null, Self::d(1, 1))]),
            row(Value::Null, Self::n(4000), Self::s("gold"), vec![(Self::n(10), t(), Self::n(1))]),
            row(Self::n(40), Self::n(4000), Value::Null, vec![(Self::n(10), t(), Self::n(1))]),
            row(Self::n(40), Value::Null, Self::s("bronze"), vec![(Self::n(10), t(), Self::n(1))]),
            row(Self::n(40), Self::n(4000), Self::s("silver"), vec![(Self::n(10), t(), Value::Null)]),
            row(Self::n(40), Self::n(4000), Self::s("silver"), Vec::new()),
            row(
                Self::n(40),
                Self::d(15, 1),
                Self::s("gold"),
                vec![(Self::d(125, 1), t(), Self::d(25, 2)), (Self::d(1, 9), t(), Self::d(1, 28)), (Self::n(7), Value::Bool(false), Self::n(0))],
            ),
            row(Self::n(40), Self::n(4000), Self::s("gold"), vec![(Self::n(9_000_000_000_000_000_000), t(), Self::n(-9_000_000_000_000_000_000))]),
            row(Self::d(175, 1), Self::n(10), Self::s("bronze"), vec![(Self::n(15), t(), Self::n(0))]),
            row(Self::n(18), Self::n(0), Self::s("silver"), vec![(Self::n(-1), t(), Self::n(0))]),
        ]
    }

    fn w3_extremes() -> Vec<Row> {
        let row = |score: Value, income: Value, region: Value, verified: Value| Row { columns: vec![score, income, region, verified], lists: Vec::new() };
        let t = || Value::Bool(true);
        vec![
            row(Value::Null, Self::d(5_000_000, 2), Self::s("US"), t()),
            row(Self::n(810), Value::Null, Self::s("US"), t()),
            row(Self::n(810), Self::d(i64::MAX, 2), Self::s("US"), t()),
            row(Self::n(720), Self::d(i64::MAX - 7, 2), Self::s("IN"), Value::Bool(false)),
            row(Self::n(760), Self::d(5_000_000, 2), Value::Null, Value::Null),
            row(Value::Null, Value::Null, Value::Null, Value::Null),
            row(Self::n(450), Self::d(1, 2), Self::s("JP"), Value::Bool(false)),
            row(Self::n(i64::MAX), Self::d(-5, 2), Self::s("US"), t()),
            row(Self::n(690), Self::d(9_000_000_000, 2), Self::s("JP"), t()),
        ]
    }

    fn w4_extremes() -> Vec<Row> {
        let row = |qty: Value, price: Value, discount: Value, weight: Value, express: Value, country: Value, sku: Value| Row {
            columns: vec![qty, price, discount, weight, express, country, sku],
            lists: Vec::new(),
        };
        let sku = || Self::s("PRO-1");
        vec![
            row(Self::n(i64::MAX / 10), Self::d(100_000, 2), Self::n(5), Self::d(10, 1), Value::Bool(false), Self::s("US"), sku()),
            row(Self::n(2), Self::d(1000, 2), Value::Null, Self::d(10, 1), Value::Bool(true), Self::s("CA"), Value::Null),
            row(Self::n(2), Self::d(1000, 2), Self::n(3), Value::Null, Value::Bool(true), Self::s("CA"), sku()),
            row(Self::n(2), Self::d(1000, 2), Self::n(3), Self::d(10, 1), Value::Bool(true), Value::Null, Self::s("")),
            row(Self::n(2), Self::d(1000, 2), Self::n(3), Self::d(10, 1), Value::Null, Self::s("MX"), Self::s("STD-9")),
            row(Self::n(2), Self::d(1000, 2), Self::n(3), Self::d(300, 1), Value::Null, Self::s("MX"), Self::s("PROMO-77")),
            row(Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null),
            row(Self::n(1), Self::d(9_000_000_000_000_000_000, 2), Self::n(100), Self::d(1, 1), Value::Bool(false), Self::s("US"), sku()),
            row(Self::n(3), Self::d(4_000_000_000_000_000_000, 2), Self::n(7), Self::d(1, 1), Value::Bool(false), Self::s("US"), sku()),
            row(Self::n(-4), Self::d(2500, 2), Self::n(-10), Self::d(-5, 1), Value::Bool(true), Self::s("GB"), Self::s("PRÖ-ü")),
        ]
    }
}
