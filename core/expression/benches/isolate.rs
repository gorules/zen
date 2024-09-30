use std::ops::Index;

use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use csv::StringRecord;

use zen_expression::variable::Variable;
use zen_expression::Isolate;

fn bench_unary(b: &mut Bencher, source: &'static str) {
    let s = serde_json::from_str(r#"{ "$": "ru" }"#).unwrap();

    let mut isolate = Isolate::with_environment(s);
    b.iter(|| {
        criterion::black_box(isolate.run_unary(source).unwrap());
    })
}

fn bench_standard(b: &mut Bencher, source: &'static str) {
    let s = serde_json::from_str(r#"{ "$": "ru" }"#).unwrap();

    let mut isolate = Isolate::with_environment(s);
    b.iter(|| {
        criterion::black_box(isolate.run_standard(source).unwrap());
    })
}

enum BenchmarkKind {
    Unary,
    Standard,
}

fn bench_csv(b: &mut Bencher, kind: BenchmarkKind, csv_data: &'static str) {
    let maybe_rows: Result<Vec<StringRecord>, _> = csv::ReaderBuilder::new()
        .delimiter(b';')
        .flexible(true)
        .has_headers(true)
        .from_reader(csv_data.as_bytes())
        .into_records()
        .collect();

    let rows = maybe_rows.expect("Must be valid");

    struct TestCase {
        expression: String,
        environment: Option<Variable>,
    }

    let test_cases: Vec<TestCase> = rows
        .iter()
        .filter_map(|row| {
            if row.len() < 2 {
                return None;
            }

            let (expression, input_str) = (row.index(0), row.index(1));
            if expression.starts_with("#") {
                return None;
            }

            let mut case = TestCase {
                expression: expression.to_string(),
                environment: None,
            };
            if !input_str.is_empty() {
                case.environment = Some(serde_json5::from_str(input_str).unwrap());
            }

            return Some(case);
        })
        .collect();

    let mut isolate = Isolate::new();

    b.iter(|| {
        for TestCase {
            expression,
            environment,
        } in &test_cases
        {
            if let Some(env) = environment {
                isolate.set_environment(env.clone());
            }

            match kind {
                BenchmarkKind::Unary => {
                    criterion::black_box(isolate.run_unary(expression).unwrap());
                }
                BenchmarkKind::Standard => {
                    criterion::black_box(isolate.run_standard(expression).unwrap());
                }
            };
        }
    });
}

fn bench_functions(c: &mut Criterion) {
    c.bench_function("isolate/simple", |b| {
        bench_unary(b, "'ru', 'se'");
    });

    c.bench_function("isolate/standard", |b| {
        bench_standard(b, "contains(['ru', 'se'], $)");
    });

    c.bench_function("isolate/template-string", |b| {
        bench_standard(b, "`first ${1} second ${2} third ${3}`");
    });

    c.bench_function("isolate/csv-standard", |b| {
        bench_csv(
            b,
            BenchmarkKind::Standard,
            include_str!("../tests/data/standard.csv"),
        )
    });

    c.bench_function("isolate/csv-unary", |b| {
        bench_csv(
            b,
            BenchmarkKind::Unary,
            include_str!("../tests/data/unary.csv"),
        )
    });
}

criterion_group!(benches, bench_functions);
criterion_main!(benches);
