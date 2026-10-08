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
    pub(crate) const NAMES: [&'static str; 4] = ["W1", "W2", "W3", "W4"];

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
            _ => Self::w4_rows(count),
        }
    }

    pub(crate) fn extremes(&self) -> Vec<Row> {
        match self.name {
            "W1" => Self::w1_extremes(),
            "W2" => Self::w2_extremes(),
            "W3" => Self::w3_extremes(),
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
