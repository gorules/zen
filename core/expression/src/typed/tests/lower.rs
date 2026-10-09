use crate::lane::Fusion;
use crate::typed::tests::data::{ColSpec, Enc, ListSpec, Mock, Spec};
use crate::typed::{Class, DictKind, Hir, Hit, Kind, Lit, Lowering, Op, Program, Resolve, Resolved, TableRule, TableSpec};
use std::sync::Arc;

struct Anchor;

enum Unit {
    Fused(Vec<Fusion>),
    Table(TableSpec),
}

impl Anchor {
    fn spec() -> Spec {
        Spec {
            columns: vec![
                ColSpec::new("customer.age", Class::Num, Enc::Scaled),
                ColSpec::new("customer.income", Class::Num, Enc::Scaled),
                ColSpec::new("customer.tier", Class::Text, Enc::Text),
                ColSpec::new("customer.rate", Class::Num, Enc::Scaled),
            ],
            lists: vec![ListSpec {
                parent: None,
                path: "customer.accounts",
                alias: "account",
                fields: vec![
                    ColSpec::new("balance", Class::Num, Enc::Scaled),
                    ColSpec::new("kind", Class::Text, Enc::Text),
                    ColSpec::new("active", Class::Bool, Enc::Bool),
                    ColSpec::new("fee", Class::Num, Enc::Scaled),
                    ColSpec::new("label", Class::Text, Enc::Text),
                ],
            }],
        }
    }

    fn out(key: &str, source: &str) -> Unit {
        Unit::Fused(vec![Fusion::Output { key: key.to_string(), source: source.to_string() }])
    }

    fn member() -> Vec<Unit> {
        vec![
            Self::out("account.fee", "account.balance * customer.rate / 100 * (account.kind == 'loan' ? 2 : 1)"),
            Self::out("account.label", "((account.kind == 'savings') == true) ? ('S') : (((account.kind == 'checking') == true) ? ('C') : ('L'))"),
        ]
    }

    fn root() -> Vec<Unit> {
        let cell = |cell: &str| (!cell.is_empty()).then(|| cell.to_string());
        let rule = |tier: &str, risk: &str, value: &str| TableRule { cells: vec![cell(tier), cell(risk)], values: vec![Some(value.to_string())] };
        vec![
            Self::out("customer.total", "sum(map(filter(customer.accounts, #.active), #.balance))"),
            Self::out("customer.fees", "sum(map(customer.accounts, #.fee))"),
            Self::out(
                "customer.risk",
                "((customer.total > customer.income * 2) == true) ? ('high') : (((customer.total > customer.income) == true) ? ('mid') : ('low'))",
            ),
            Unit::Table(TableSpec {
                inputs: vec![Some("customer.tier".to_string()), Some("customer.risk".to_string())],
                outputs: vec!["customer.discount".to_string()],
                rules: vec![
                    rule("'gold'", "'low'", "0.15"),
                    rule("'gold'", "'mid'", "0.1"),
                    rule("'silver'", "'low'", "0.1"),
                    rule("'silver'", "'mid'", "0.05"),
                    rule("", "'low'", "0.02"),
                    rule("", "", "0"),
                ],
                hit: Hit::Strict,
                selector: false,
            }),
            Self::out("customer.net", "customer.total - customer.fees - customer.discount"),
            Unit::Fused(vec![
                Fusion::Output { key: String::new(), source: "customer.age >= 18".to_string() },
                Fusion::Output { key: String::new(), source: "customer.risk != 'high'".to_string() },
            ]),
        ]
    }

    fn lower(member: bool, units: Vec<Unit>) -> Hir {
        let spec = Self::spec();
        let mut lowering = Lowering::new(member.then_some(("account", "customer.accounts")));
        let mut mock = Mock { spec: &spec, member: member.then_some(0) };
        for unit in units {
            match unit {
                Unit::Fused(entries) => lowering.unit(&entries, &mut mock),
                Unit::Table(table) => lowering.table(&table, &mut mock),
            }
            .expect("unit");
        }
        lowering.finish()
    }

    const MEMBER: &'static str = r#"column 0 customer.rate : num/*?
list 0 customer.accounts as account
  field 0 balance : num/*?
  field 1 kind : text?
dict 0 Classes ["loan", "savings", "checking"]
dict 1 Literals ["C", "L", "S"]
%0 = field 0.balance : num/*? @elem0
%1 = column customer.rate : num/*? @row
%2 = parent %1 : num/*? @elem0
%3 = mul %0, %2 : num/* @elem0
%4 = const 100 : num/0 @row
%5 = div %3, %4 : num/* @elem0
%6 = field 0.kind : text? @elem0
%7 = classify %6 dict 0 : code#0? @elem0
%8 = const code#0:0 : code#0 @row
%9 = eq %7, %8 : bool @elem0
%10 = const 2 : num/0 @row
%11 = const 1 : num/0 @row
%12 = if %9 then %10 else %11 : num/0 @elem0
%13 = mul %5, %12 : num/* @elem0
%14 = const code#0:1 : code#0 @row
%15 = eq %7, %14 : bool @elem0
%16 = const code#0:2 : code#0 @row
%17 = eq %7, %16 : bool @elem0
%18 = const code#1:0 : code#1 @row
%19 = const code#1:1 : code#1 @row
%20 = if %17 then %18 else %19 : code#1 @elem0
%21 = const code#1:2 : code#1 @row
%22 = if %15 then %21 else %20 : code#1 @elem0
out 0 "account.fee" = %13 : num/* @elem0
out 1 "account.label" = %22 : code#1 @elem0
"#;

    const ROOT: &'static str = r#"column 0 customer.income : num/*?
column 1 customer.tier : text?
column 2 customer.age : num/*?
list 0 customer.accounts as -
  field 0 balance : num/*?
  field 1 kind : text?
  field 2 active : bool?
  field 3 fee : num/*?
  field 4 label : text?
dict 0 Literals ["mid", "low", "high"]
dict 1 Classes ["gold", "silver"]
%0 = field 0.active : bool? @elem0
%1 = field 0.balance : num/*? @elem0
%2 = fold sum list 0 filter %0 value %1 : num/* @row
%3 = field 0.fee : num/*? @elem0
%4 = fold sum list 0 filter - value %3 : num/* @row
%5 = column customer.income : num/*? @row
%6 = const 2 : num/0 @row
%7 = mul %5, %6 : num/* @row
%8 = gt %2, %7 : bool @row
%9 = const true : bool @row
%10 = gt %2, %5 : bool @row
%11 = const code#0:0 : code#0 @row
%12 = const code#0:1 : code#0 @row
%13 = if %10 then %11 else %12 : code#0 @row
%14 = const code#0:2 : code#0 @row
%15 = if %8 then %14 else %13 : code#0 @row
%16 = column customer.tier : text? @row
%17 = classify %16 dict 1 : code#1? @row
%18 = const code#1:0 : code#1 @row
%19 = eq %17, %18 : bool @row
%20 = eq %15, %12 : bool @row
%21 = and %19, %20 : bool @row
%22 = eq %15, %11 : bool @row
%23 = and %19, %22 : bool @row
%24 = const code#1:1 : code#1 @row
%25 = eq %17, %24 : bool @row
%26 = and %25, %20 : bool @row
%27 = and %25, %22 : bool @row
%28 = firstmatch strict [%21, %23, %26, %27, %20, %9] : num/0 @row
%29 = const 0.15 : num/2 @row
%30 = const 0.1 : num/1 @row
%31 = const 0.05 : num/2 @row
%32 = const 0.02 : num/2 @row
%33 = const 0 : num/0 @row
%34 = pick %28 [%29, %30, %30, %31, %32, %33] : num/* @row
%35 = sub %2, %4 : num/* @row
%36 = sub %35, %34 : num/* @row
%37 = column customer.age : num/*? @row
%38 = const 18 : num/0 @row
%39 = ge %37, %38 : bool @row
%40 = ne %15, %14 : bool @row
out 0 "customer.total" = %2 : num/* @row
out 1 "customer.fees" = %4 : num/* @row
out 2 "customer.risk" = %15 : code#0 @row
out 3 "customer.discount" = %34 : num/* @row
out 4 "customer.net" = %36 : num/* @row
out 5 "" = %39 : bool @row
out 6 "" = %40 : bool @row
"#;
}

struct Probe {
    asked: Vec<String>,
}

impl Resolve for Probe {
    fn path(&mut self, path: &str) -> Resolved {
        self.asked.push(path.to_string());
        match path {
            "c.n" | "c.m" => Resolved::Column(Class::Num),
            "c.t" | "c.u" => Resolved::Column(Class::Text),
            "c.b" => Resolved::Column(Class::Bool),
            "c.f" => Resolved::Column(Class::Num),
            "c.weird" => Resolved::Unsupported,
            "c.xs" => Resolved::List { fields: vec![(Arc::from(""), Class::Num)] },
            "c.ts" => Resolved::List { fields: vec![(Arc::from(""), Class::Text)] },
            "c.items" => Resolved::List { fields: vec![(Arc::from("q"), Class::Num), (Arc::from("n"), Class::Text)] },
            _ => Resolved::Absent,
        }
    }

    fn alias(&mut self, _: &str) -> Resolved {
        Resolved::Unsupported
    }

    fn nested(&mut self, chain: &[&str]) -> Resolved {
        match chain {
            ["c.items", "subs", ..] => Resolved::List { fields: vec![(Arc::from("v"), Class::Num)] },
            _ => Resolved::Unsupported,
        }
    }
}

impl Probe {
    fn new() -> Self {
        Self { asked: Vec::new() }
    }

    fn unit(lowering: &mut Lowering, probe: &mut Probe, key: &str, source: &str) -> Result<(), crate::typed::Unsupported> {
        lowering.unit(&[Fusion::Output { key: key.to_string(), source: source.to_string() }], probe)
    }
}

#[test]
fn anchor_member_group() {
    let hir = Anchor::lower(true, Anchor::member());
    assert_eq!(hir.to_string(), Anchor::MEMBER);
}

#[test]
fn anchor_root_group() {
    let hir = Anchor::lower(false, Anchor::root());
    assert_eq!(hir.to_string(), Anchor::ROOT);
}

#[test]
fn unit_rolls_back() {
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    Probe::unit(&mut lowering, &mut probe, "c.x", "c.n + 1").expect("first");
    let failed = lowering.unit(
        &[
            Fusion::Output { key: "c.y".to_string(), source: "c.m * 2".to_string() },
            Fusion::Output { key: "c.z".to_string(), source: "c.t == 'a' ? c.weird : 1".to_string() },
        ],
        &mut probe,
    );
    assert!(failed.is_err());
    assert_eq!((lowering.units(), lowering.outputs()), (1, 1));
    Probe::unit(&mut lowering, &mut probe, "c.w", "c.y ?? c.x").expect("third");
    let hir = lowering.finish();
    assert_eq!(hir.outputs.len(), 2);
    assert!(hir.schema.columns.iter().all(|c| c.path != "c.m" && c.path != "c.t"), "{hir}");
    assert!(hir.schema.dicts.is_empty(), "{hir}");
    assert!(probe.asked.contains(&"c.y".to_string()));
}

#[test]
fn text_compares_classify() {
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    Probe::unit(&mut lowering, &mut probe, "c.a", "c.t == 'gold'").expect("eq");
    Probe::unit(&mut lowering, &mut probe, "c.b", "c.t in ['gold', 'silver']").expect("in");
    Probe::unit(&mut lowering, &mut probe, "c.c", "'x' != c.t").expect("ne");
    Probe::unit(&mut lowering, &mut probe, "c.d", "c.t == null").expect("null");
    Probe::unit(&mut lowering, &mut probe, "c.e", "c.u not in ['a']").expect("other column");
    let hir = lowering.finish();
    let classify: Vec<_> = hir.nodes.iter().filter(|n| matches!(n.op, Op::Classify(..))).collect();
    assert_eq!(classify.len(), 2, "{hir}");
    assert!(hir.schema.dicts.iter().all(|d| d.kind == DictKind::Classes), "{hir}");
    assert_eq!(hir.schema.dicts[0].values.iter().map(|v| v.as_ref()).collect::<Vec<_>>(), ["gold", "silver", "x"]);
    assert!(hir.outputs.iter().all(|o| o.ty.kind == Kind::Bool));
}

#[test]
fn strict_rules_lower_to_firstmatch() {
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    let table = TableSpec {
        inputs: vec![Some("c.n".to_string())],
        outputs: vec!["c.r".to_string()],
        rules: vec![
            TableRule { cells: vec![Some("> 5".to_string())], values: vec![Some("'hi'".to_string())] },
            TableRule { cells: vec![None], values: vec![Some("'lo'".to_string())] },
        ],
        hit: Hit::Strict,
        selector: false,
    };
    lowering.table(&table, &mut probe).expect("rules");
    let hir = lowering.finish();
    assert!(hir.nodes.iter().any(|n| matches!(n.op, Op::FirstMatch { strict: true, .. })), "{hir}");
    assert!(matches!(hir.outputs[0].ty.kind, Kind::Code(_)));
    assert!(!hir.outputs[0].ty.nullable, "{hir}");
}

#[test]
fn unsupported_shapes() {
    let cases = [
        ("text ordering", "c.t > 'a'"),
        ("text plus number", "c.t + 1"),
        ("text plus null", "c.t + null"),
        ("pattern from a column", "matches(c.t, c.u)"),
        ("invalid pattern", "matches(c.t, '(')"),
        ("string of a list", "string(c.xs)"),
        ("empty template", "``"),
        ("unsupported path", "c.weird + 1"),
        ("null arithmetic", "c.absent + 1"),
        ("variable exponent", "c.n ^ c.n"),
        ("fractional exponent", "c.n ^ 0.5"),
        ("round to variable places", "round(c.n, c.n)"),
        ("bool of a number", "bool(c.n)"),
        ("min of nothing", "min([])"),
        ("closure over a column", "sum(map(c.n, #))"),
    ];
    for (what, source) in cases {
        let mut lowering = Lowering::new(None);
        let mut probe = Probe::new();
        assert!(Probe::unit(&mut lowering, &mut probe, "c.o", source).is_err(), "{what}: {source}");
        assert_eq!(lowering.outputs(), 0);
    }
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    Probe::unit(&mut lowering, &mut probe, "c.o", "c.n + 1").expect("write");
    assert!(Probe::unit(&mut lowering, &mut probe, "c.p", "c.o.x").is_err(), "read inside a written value");
    assert!(Probe::unit(&mut lowering, &mut probe, "c.o.x", "1").is_ok());
    assert!(Probe::unit(&mut lowering, &mut probe, "c.q", "c.o").is_err(), "read of a partially written value");
}

#[test]
fn text_shapes() {
    let supported = [
        "c.t == c.u",
        "c.t",
        "c.b ? c.t : 'x'",
        "(c.n > 1 ? 'a' : 'b') == c.t",
        "string(c.n)",
        "`${c.t}:${c.n}`",
        "c.t + '-' + upper(c.u)",
        "contains(lower(c.t), 'x')",
        "trim(c.t) in ['a', 'b']",
        "matches(c.t, '^a+$')",
        "c.t ?? 'none'",
        "map(c.items, #.n)",
        "count(c.items, #.n == c.t)",
        "c.t in c.ts",
        "map(c.ts, upper(#))",
        "len(c.t + c.u)",
    ];
    for source in supported {
        let mut lowering = Lowering::new(None);
        let mut probe = Probe::new();
        assert!(Probe::unit(&mut lowering, &mut probe, "c.o", source).is_ok(), "{source}");
        let hir = lowering.finish();
        Program::new(hir.clone()).unwrap_or_else(|e| panic!("{source}: {e}\n{hir}"));
    }
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    Probe::unit(&mut lowering, &mut probe, "c.a", "'a' + 'b' + string(1.50) + `x${true}`").expect("constant");
    Probe::unit(&mut lowering, &mut probe, "c.b", "upper('ß') + trim('  q ')").expect("constant map");
    let hir = lowering.finish();
    let dict = &hir.schema.dicts[0];
    let texts: Vec<&str> = hir.outputs.iter().map(|o| match hir.node(o.node).op {
        Op::Const(Lit::Code(_, c)) => dict.text(c).unwrap_or("?"),
        _ => "?",
    }).collect();
    assert_eq!(texts, ["ab1.50xtrue", "SSq"], "{hir}");
}

#[test]
fn list_shapes() {
    let supported = [
        "sum(c.xs)",
        "filter(c.xs, # > 1)",
        "map(c.items, #.q * 2)",
        "c.n in c.xs",
        "'a' in c.ts",
        "contains(c.xs, 1)",
        "count(filter(c.items, #.q > 1), #.q < 5)",
        "sum(map(c.items, sum(map(#.subs, #.v))))",
        "c.xs ?? []",
        "len(filter(c.xs, # > 0))",
    ];
    for source in supported {
        let mut lowering = Lowering::new(None);
        let mut probe = Probe::new();
        assert!(Probe::unit(&mut lowering, &mut probe, "c.o", source).is_ok(), "{source}");
        let hir = lowering.finish();
        Program::new(hir.clone()).unwrap_or_else(|e| panic!("{source}: {e}\n{hir}"));
    }
    let unsupported = [
        ("filter over a computed map", "filter(map(c.xs, # * 2), # > 1)"),
        ("quantifier over a computed map", "some(map(c.xs, # * 2), # > 1)"),
        ("membership in a computed map", "1 in map(c.xs, # * 2)"),
        ("objects as values", "filter(c.items, #.q > 1)"),
        ("sum of objects", "sum(c.items)"),
        ("list as a scalar", "c.xs + 1"),
        ("nested too deep", "sum(map(c.items, sum(map(#.subs, sum(map(#.deep, sum(map(#.deeper, #.v))))))))"),
    ];
    for (what, source) in unsupported {
        let mut lowering = Lowering::new(None);
        let mut probe = Probe::new();
        assert!(Probe::unit(&mut lowering, &mut probe, "c.o", source).is_err(), "{what}: {source}");
        assert_eq!(lowering.outputs(), 0);
        assert!(lowering.finish().schema.lists.is_empty(), "{what}: rolled back");
    }
}

#[test]
fn absent_paths_read_null() {
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    Probe::unit(&mut lowering, &mut probe, "c.o", "c.missing ?? c.n").expect("coalesce");
    Probe::unit(&mut lowering, &mut probe, "c.p", "c.missing == null").expect("compare");
    let hir = lowering.finish();
    assert_eq!(hir.schema.columns.len(), 1, "{hir}");
    assert_eq!(probe.asked.iter().filter(|p| *p == "c.missing").count(), 1);
}

#[test]
fn date_shapes() {
    let supported = [
        "d(c.t)",
        "d()",
        "d(c.n, 'Europe/Berlin')",
        "d(c.t).add(c.n, 'day').format('%Y-%m-%d')",
        "d(c.t).diff(c.u, 'month')",
        "d(c.t) < d()",
        "c.t >= d('2024-01-01')",
        "d(c.t) == c.u",
        "d(c.t) in [d('2024-01-01'), '2024-02-01', null]",
        "d(c.t) in [d('2024-01-01')..d('2024-12-31'))",
        "`${d(c.t)}`",
        "'at ' + d(c.t)",
        "count(c.ts, # < d())",
        "max(map(c.ts, d(#)))",
        "min([d(c.t), d(c.u)])",
        "c.b ? d(c.t) : null",
        "year(c.t) + date(c.n)",
        "dateString(d(c.t))",
    ];
    for source in supported {
        let mut lowering = Lowering::new(None);
        let mut probe = Probe::new();
        assert!(Probe::unit(&mut lowering, &mut probe, "c.o", source).is_ok(), "{source}");
        let hir = lowering.finish();
        Program::new(hir.clone()).unwrap_or_else(|e| panic!("{source}: {e}\n{hir}"));
    }
    let unsupported = [
        ("method on text", "c.t.year()"),
        ("format from a column", "d(c.t).format(c.u)"),
        ("invalid format", "d(c.t).format('%Q')"),
        ("method arity", "d(c.t).add()"),
        ("constructor arity", "d(c.t, 'UTC', 1)"),
        ("ordering with a number", "d(c.t) < c.n"),
        ("date plus number", "d(c.t) + 1"),
        ("text interval", "c.t in [d('2024-01-01')..d('2024-12-31')]"),
        ("mixed branches", "c.b ? d(c.t) : c.t"),
        ("non-literal set item", "d(c.t) in [d(c.u)]"),
    ];
    for (what, source) in unsupported {
        let mut lowering = Lowering::new(None);
        let mut probe = Probe::new();
        assert!(Probe::unit(&mut lowering, &mut probe, "c.o", source).is_err(), "{what}: {source}");
    }
    let mut lowering = Lowering::new(None);
    let mut probe = Probe::new();
    Probe::unit(&mut lowering, &mut probe, "c.a", "d('2024-01-31T10:00:00Z').add(1, 'month').format('%Y-%m-%d')").expect("constant method");
    Probe::unit(&mut lowering, &mut probe, "c.b", "date('2024-01-01')").expect("constant legacy");
    Probe::unit(&mut lowering, &mut probe, "c.c", "d('nope').isValid()").expect("invalid constant");
    Probe::unit(&mut lowering, &mut probe, "c.d", "date('now')").expect("legacy now");
    let hir = lowering.finish();
    let consts: Vec<String> = hir.outputs.iter().map(|o| match &hir.node(o.node).op {
        Op::Const(Lit::Code(d, c)) => hir.dict(*d).text(*c).unwrap_or("?").to_string(),
        Op::Const(lit) => lit.to_string(),
        op => format!("{op:?}"),
    }).collect();
    assert_eq!(consts[..3], ["2024-02-29", "1704067200", "false"], "{hir}");
    assert!(matches!(hir.node(hir.outputs[3].node).op, Op::Host(..)), "{hir}");
}
