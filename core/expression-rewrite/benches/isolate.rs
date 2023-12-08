use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use serde_json::Value;
use zen_expression_rewrite::Isolate;


fn bench_source(b: &mut Bencher, source: &'static str) {
    let s: Value = serde_json::from_str(r#"{ "$": "ru" }"#).unwrap();

    let mut isolate = Isolate::with_environment(&s);
    b.iter(|| {
        criterion::black_box(isolate.run_unary(source).unwrap());
    })
}

fn bench_standard(b: &mut Bencher, source: &'static str) {
    let s: Value = serde_json::from_str(r#"{ "$": "ru" }"#).unwrap();

    let mut isolate = Isolate::with_environment(&s);
    b.iter(|| {
        criterion::black_box(isolate.run_standard(source).unwrap());
    })
}

fn bench_functions(c: &mut Criterion) {
    c.bench_function("isolate/simple", |b| {
        bench_source(b, "'ru', 'se'");
    });

    c.bench_function("isolate/standard", |b| {
        bench_standard(b, "contains(['ru', 'se', 'b', 'c', 'd', 'e'], $)");
    });
}

criterion_group!(benches, bench_functions);
criterion_main!(benches);
