use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use serde_json::Value;

use zen_expression::isolate::Isolate;

fn bench_source(b: &mut Bencher, source: &'static str) {
    let s: Value = serde_json::from_str(r#"{ "$": "ru" }"#).unwrap();

    let isolate = Isolate::default();
    isolate.inject_env(&s);

    b.iter(|| {
        criterion::black_box(isolate.run_unary(source).unwrap());
    })
}

fn bench_functions(c: &mut Criterion) {
    c.bench_function("isolate/simple", |b| {
        bench_source(b, "'ru', 'se'");
    });
}

criterion_group!(benches, bench_functions);
criterion_main!(benches);
