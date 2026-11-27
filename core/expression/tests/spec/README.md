# ZEN expression conformance suite

Executable specification of the ZEN expression language, in the spirit of test262.
Each case states what the language is meant to produce, not only what the current VM
does. Known deviations are recorded in place.

## Layout

- `standard/NN-chapter.csv`: standard expressions (`Isolate::run_standard`)
- `unary/NN-chapter.csv`: unary expressions (`Isolate::run_unary`, reference in `$`)

## Row format

`expression;input;output;note`, `;`-separated CSV. Fields containing `;` or `"` are
wrapped in double quotes with inner `"` doubled. Lines starting with `#` are section
comments.

- **expression**: source text. It cannot start with `#`.
- **input**: the environment as a JSON5-like literal, or empty for none. Numbers are read
  as exact decimals, so `0.1000000000000000000000000001` is kept. `date('...')` is a date
  input that keeps its source text, as the engine produces for schema date fields
  (`DateValue::from_text`).
- **output**: the expected value as a literal, or `!error` (any error). Numbers are
  compared by exact decimal value (`1.10 == 1.1`); scale is observed through `string()`
  or templates. Dates are written as their string form
  (`'2024-01-01T00:00:00Z'`).
- **note** (optional):
  - `bug: <why>`: the output column holds the intended result and the current VM
    differs.
  - `question: <why>`: the output column holds current behaviour whose intent is
    unclear.

## Runners

- `cargo test -p zen-expression --test spec`: the stack VM against the spec. A `bug:`
  case that starts passing fails the run, so the note gets removed.
- `cargo test -p zen-expression --test lane lane_matches_spec`: every lane path (row,
  batch, specialized, typed, columns) against the stack VM's actual result, bugs
  included.

Environment knobs:
- `SPEC_FILE=<substring>`: run only matching files.
- `SPEC_SHOW=<n>`: how many failures to print.
- `SPEC_BUGS=1`: list known bugs.
- `SPEC_TZ=<zone>`: local time zone (default UTC).

Evaluation time is fixed at `2025-03-20T10:15:30Z`.
