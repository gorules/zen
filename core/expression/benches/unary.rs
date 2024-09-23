use bumpalo::Bump;
use criterion::{criterion_group, criterion_main, Bencher, Criterion};

use zen_expression::lexer::Lexer;
use zen_expression::parser::Parser;

fn bench_source(b: &mut Bencher, src: &'static str) {
    let mut lexer = Lexer::new();
    let mut bump = Bump::new();
    let tokens = lexer.tokenize(src).unwrap();

    b.iter(|| {
        let unary_parser = Parser::try_new(tokens, &bump).unwrap().unary();
        criterion::black_box(unary_parser.parse());

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
