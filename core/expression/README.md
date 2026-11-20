# ZEN Expression Language

**The expression language of [ZEN Engine](https://crates.io/crates/zen-engine)**, the open-source [Business Rules Engine (BRE)](https://gorules.io) from [GoRules](https://gorules.io).

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![crates.io](https://img.shields.io/crates/v/zen-expression.svg)](https://crates.io/crates/zen-expression)

ZEN expressions are readable by business users and fast enough for hot paths: a complete language for conditions, calculations and data transformation, with lexer, parser, compiler, VM, type checking and editor completions included in this crate.

```rust
use serde_json::json;
use zen_expression::evaluate_expression;

fn main() {
    let result = evaluate_expression(
        "sum(items) * multiplier",
        json!({ "items": [10, 20, 30], "multiplier": 2 }).into(),
    )
    .unwrap();
    // => 120
}
```

Reusable expressions compile once via `compile_expression` and evaluate repeatedly against different contexts.

## Evaluation budget

By default an expression can run for as long as it needs and expand an interval to any size, which is fine
for trusted expressions. When expressions come from untrusted sources, two global limits bound one evaluation
(`0` = no limit, the default):

```rust
use std::sync::atomic::Ordering;
use zen_expression::EXPRESSION_CONFIG;

EXPRESSION_CONFIG.max_steps.store(1_000_000, Ordering::Relaxed);       // VM instructions per evaluation
EXPRESSION_CONFIG.max_interval_length.store(100_000, Ordering::Relaxed); // elements of an expanded `[a..b]`
```

When a limit is exceeded the evaluation fails with `VMError::BudgetExceeded`. Membership tests such as
`x in [1..10000]` do not expand the interval and are not affected by `max_interval_length`.

## Resources

- [ZEN Expression Language reference](https://docs.gorules.io/learn/zen-language/syntax) - syntax, [operators](https://docs.gorules.io/learn/zen-language/operators), [built-in functions](https://docs.gorules.io/learn/zen-language/functions) and [date operations](https://docs.gorules.io/learn/zen-language/dates)
- [ZEN Engine](https://github.com/gorules/zen) - the rules engine built on this language
- [GoRules](https://gorules.io) - the platform around the open-source engine
- [Online Editor](https://editor.gorules.io) - try expressions in the free editor with a built-in simulator

## License

[MIT License](https://opensource.org/licenses/MIT)
