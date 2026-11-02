# ZEN engine conformance suite

Executable specification of decision graphs and policies, the engine-level sibling of
`core/expression/tests/spec`. Each case states what a decision is meant to produce, not
only what the current engine does. Known deviations are recorded in place.

## Layout

- `graph/NN-chapter.json5`: decision graphs (JDM), evaluated by the graph walker.
- `policy/NN-chapter.json5`: policies, evaluated by the policy driver.

A chapter is a sequence of suites. A suite is one decision plus the cases run against it.
Files use a JSON5-like syntax: `//` and `/* */` comments, unquoted keys, single or double
quoted strings, trailing commas, and `"""raw"""` strings for multi-line source (function
nodes). Numbers are read as exact decimals. `date('...')` is a date input that keeps its
source text, as the engine produces for schema date fields. Graph requests are normalized at
the input node, so in graph suites a `date()` input arrives as text; build date values with an
upstream `d(...)` expression or an input schema field with `format: date`.

## Cases

```json5
cases: [
  [input, output],
  [input, output, "bug: why the engine differs"],
  {input: {...}, output: {...}, goals: ["customer.tier"], note: "question: ..."},
]
```

- **output**: the expected result, compared by value. Numbers compare by exact decimal
  value (`1.10 == 1.1`); objects compare unordered. `"!error"` expects any error;
  `"!error: text"` expects an error whose serialized form contains `text` (use it only for
  stable, user-facing error kinds).
- **note** (optional):
  - `bug: <why>`: the output holds the intended result and the engine differs. A bug case
    that starts passing fails the run, so the note gets removed.
  - `question: <why>`: the output holds current behaviour whose intent is unclear.
- **goals** (policy only): the request goals.
- **engines** (object form only): a documented difference between the reference (walker or
  driver) and the other engine modes, with the reason. The case is still checked against the
  spec, but not compared across engines. Use it only when the reference itself is wrong or
  reports a different but equivalent error, and say which side gives the intended result.

Evaluation time is fixed at `2025-03-20T10:15:30Z`, time zone UTC (`SPEC_TZ` overrides).

## Graph suites

```json5
{
  name: "first hit returns the first matching rule",
  graph: {
    nodes: [
      {table: "rules", hit: "first", inputs: ["age"], outputs: ["tier"], rules: [
        ["> 18", "'adult'"],
        ["", "'minor'"],
      ]},
    ],
  },
  cases: [
    [{age: 20}, {tier: "adult"}],
  ],
}
```

`graph` is either `{raw: <full JDM content>}` or the shorthand below. Shorthand nodes name
their kind with one key whose value is the node name (also its id unless `id` is given):

| node | fields |
|---|---|
| `{input: "request", schema?}` | `schema` is a JSON schema object or string |
| `{output: "response", schema?}` | |
| `{expression: "calc", fields}` | `fields` is `{key: "expr"}` in order, or `[["key", "expr"], ...]` for duplicate keys |
| `{table: "t", hit?, inputs, outputs, rules}` | `hit` is `first` (default) or `collect`; `inputs` are field strings (`null`/`""` = no field) or `{field, name}`; `outputs` are field paths (a `[]` suffix collects per column) or `{field, type}`; `rules` are rows of cell strings, inputs then outputs (`""` = any) |
| `{switch: "s", hit?, when: ["cond", ...]}` | `""` is the default branch; edges leave branch `i` as `"s:i"` |
| `{function: "f", source}` | v2 function node (`export const handler = async (input) => ...`) |
| `{functionV1: "f", source}` | v1 function node |
| `{decision: "d", key}` | sub-decision loaded from the suite's `decisions` |
| `{custom: "c", kind, config}` | spec adapter kinds: `echo` (returns input), `config` (returns config), `render` (each config key rendered as a template field), `fail` (errors with `config.message`) |

Expression, table and decision nodes also take `passThrough`, `inputField`, `outputPath` and
`executionMode` (`"single"` or `"loop"`).

When the graph has no input or output node, nodes named `input` and `output` are added.
`edges` is a list of chains: `"input -> a -> b -> output"`, `"route:0 -> adult"`. Without
`edges`, the nodes are chained in order from input to output.

`decisions: {key: <graph shorthand> | {raw: ...} | {policy: <policy shorthand>}}` registers
sub-decisions for decision nodes.

## Policy suites

```json5
{
  name: "an expression block writes a computed property",
  policy: {
    models: {customer: {age: "number", companies: "company[]"}, company: {revenue: "number"}},
    blocks: [
      {expression: "customer.adult", value: "customer.age >= 18"},
    ],
  },
  cases: [
    [{customer: {age: 20, companies: []}}, {customer: {age: 20, companies: [], adult: true}}],
  ],
}
```

`policy` is either `{raw: <PolicyDocument>}` or:

- `models: {entity: {property: "type"}}` and `globals: {name: {...}}` (global scope). Types:
  `string`, `number`, `boolean`, `date`, `string(a|b|c)` (enum), `entity` (relationship),
  `&entity` (reference); suffix `[]` for arrays and `?` for optional (`"company[]?"`).
- `dictionaries: {name: ["value", ["value", "label"], ...]}`
- `imports: ["path", ...]` naming documents in the suite's `policies`.
- `blocks`, in document order:
  - `{expression: "path", value: "expr"}`
  - `{match: "path", arms: [["condition", "value"], ["", "default"]]}`
  - `{assertion: "path", conditions: ["expr", ["or", "expr", depth], ...]}`
  - `{table: "id", hit?, inputs: ["field", ...], outputs: ["path", ...], rules: [[...]]}`
  - any block takes an explicit `id`.

`policies: {path: <policy shorthand> | {graph: <graph shorthand>}}` registers other
workspace documents. The suite's own policy is evaluated at path `main`.

Policies with error diagnostics are refused by `DecisionEngine`, so the runners gate them the
same way: every case of such a suite yields `CompilationErrors [codes]`. A suite that sets
`diagnostics: ["TYPE_MISMATCH", ...]` pins the exact set of error codes and evaluates its cases
through the workspace API, which runs policies despite errors; use it to specify runtime
behaviour of policies that only the workspace (editor) path can execute.

## Runners

All runners live in `core/engine/tests/spec.rs`. Run with `--all-features` (exact decimals
need `arbitrary_precision`):

- `graph_spec_matches_walker`: the graph walker against the spec.
- `graph_engines_match_walker`: the compiled engine (single, batch, columnar with plain,
  list and cycled column layouts, and `DecisionEngine` with a loader) against the walker's
  actual result, bugs included.
- `policy_spec_matches_driver`: the policy driver against the spec.
- `policy_engines_match_driver`: the compiled policy engine (single, batch, and
  `DecisionEngine` for cases without goals) against the driver's actual result.

A panic in any engine is reported as a failure of its suite and does not stop the run.

```
cargo test -p zen-engine --all-features --release --test spec
```

Environment knobs: `SPEC_FILE=<substring>` (files), `SPEC_SUITE=<substring>` (suite
names), `SPEC_SHOW=<n>` (failures printed), `SPEC_BUGS=1` (list known bugs).
