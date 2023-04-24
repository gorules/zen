# ZEN Expression

Zen Expression is business-first expression language used in
Zen Business rules engine by GoRules. The language is designed
to follow these principles:

- Side-effect free
- Dynamic types
- Simple syntax for broad audiences

It's primary objective is to bridge the gap between business analysts and engineers,
while providing outstanding performance and readability.

## Resources

[Documentation](https://gorules.io/docs/)

[Zen Language Playground](https://gorules.io/docs/rules-engine/expression-language#playground)

[Online Rules Editor](https://editor.gorules.io/)

## Unary tests
Unary test is a comma-separated list of simple expressions which
evaluate to a boolean value. Each comma separation is treated as
or operator. Inside unary expressions, a special symbol is available
$ which refers to a current column.

Some examples:
```js
// Given: $ = 1
1, 2, 3      // true
1            // true
>= 1         // true
< 1          // false
[0..10]      // true, (internally this is $ >= 0 and $ <= 10)
> 0 and < 10 // true

// Given: $ = 'USD'
'GBP', 'USD'          // true
'EUR'                 // false
startsWith($, "US")   // true - defaults to expression mode, comma is unavailable
endsWith($, "US")     // false - defaults to expression mode
lower($) == "usd"     // true - defaults to expression mode
```

## Standard tests

Expressions feature full capability syntax of ZEN language.
They give you access to all functions, and are most useful when
defining columns or outputs. Full syntax is also available in unary
expressions when $ is used (as it forces the expression mode).

```js
100 + 100                              // 200
10 * 5                                 // 50
10 ^ 2                                 // 100
1 in [1, 2, 3]                         // true
5 in (5..10]                           // false
sum([1, 2, 3])                         // 6
max([1, 2, 3])                         // 3

"hello" + " " + "world"                // "hello world"
len("world")                           // 5
weekdayString(date("2022-11-08"))      // "Tue"
contains("hello world", "hello")       // true
upper('john')                          // "JOHN"

some(['admin', 'user'], # == "admin")  // true
not all([100, 200, 400, 800], # in (100..800)) // false
filter([100, 200, 400, 800], # >= 200) // [200, 400, 800]
```