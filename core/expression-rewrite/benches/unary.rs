use bumpalo::Bump;
use criterion::{criterion_group, criterion_main, Bencher, Criterion};

use zen_expression_rewrite::lexer::Lexer;
use zen_expression_rewrite::parser::UnaryParser;

fn bench_source(b: &mut Bencher, src: &'static str) {
    let mut lexer = Lexer::new();
    let mut bump = Bump::new();
    let tokens = lexer.tokenize(src).unwrap();

    b.iter(|| {
        let unary_parser = UnaryParser::try_new(tokens, &bump).unwrap();
        criterion::black_box(unary_parser.parse().unwrap());

        bump.reset();
    })
}

fn bench_functions(c: &mut Criterion) {
    c.bench_function("unary/simple", |b| {
        bench_source(b, "'hello world'");
    });

    c.bench_function("unary/large", |b| {
        bench_source(b, "'a', 'b', 'c', 'd', 'e', 'f'")
    });
}

criterion_group!(benches, bench_functions);
criterion_main!(benches);
