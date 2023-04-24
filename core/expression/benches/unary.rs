use bumpalo::Bump;
use criterion::{criterion_group, criterion_main, Bencher, Criterion};

use zen_expression::lexer::Lexer;
use zen_expression::parser::UnaryParser;

fn bench_source(b: &mut Bencher, src: &'static str) {
    let lexer = Lexer::new();
    let mut bump = Bump::new();
    let t_res = lexer.tokenize(src).unwrap();
    let tokens = t_res.borrow();

    b.iter(|| {
        let unary_parser = UnaryParser::try_new(tokens.as_ref(), &bump).unwrap();
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
