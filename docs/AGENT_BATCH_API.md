# Agent batch API

What the data-hub agent calls in `zen-engine` / `zen-expression` (branch `experiment/data-hub`).
No cargo features are needed; the defaults are enough. `Variable` is `Rc`-based, so everything
below is single-threaded per call: run one engine call per task/thread, not shared across threads.

## (a) Single evaluation

```rust
impl DecisionEngine {
    pub fn compile(&self) -> Vec<CompileFailure>;
    pub async fn evaluate<K: AsRef<str>>(&self, key: K, context: Variable)
        -> Result<DecisionGraphResponse, Box<EvaluationError>>;
}
```

```rust
use std::sync::Arc;
use zen_engine::{loader::MemoryLoader, model::DecisionContent, DecisionEngine, Variable};

let loader = Arc::new(MemoryLoader::default());
loader.add("fraud.json", serde_json::from_str::<DecisionContent>(&json)?);
let engine = DecisionEngine::default().with_loader(loader);
let failures = engine.compile();
let response = engine.evaluate("fraud.json", Variable::from(input_json)).await?;
let output: serde_json::Value = response.result.to_value();
```

Call `compile()` once after the loader is set (and again after documents change). It needs a
loader that lists its keys (`MemoryLoader`, `FilesystemLoader`, `CachedLoader` do; a closure
loader does not). Keys resolve with or without `.json`. Documents that fail to compile are
listed in the returned failures and still evaluate through the uncompiled path.

## (b) Batch evaluation of one decision

```rust
impl DecisionEngine {
    pub async fn evaluate_many<K: AsRef<str>>(&self, key: K, inputs: &[Variable])
        -> Vec<Result<Variable, Box<EvaluationError>>>;
    pub async fn evaluate_batch<K: AsRef<str>>(&self, key: K, contexts: &[Variable], options: EvaluationOptions)
        -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>>;
    pub async fn evaluate_columns<'a, K: AsRef<str>>(&self, key: K, columns: &'a Columns<'a>,
        store: &'a ColumnStore<'a>, options: EvaluationOptions) -> ColumnarOutput<'a>;
}
```

```rust
let inputs: Vec<Variable> = rows.into_iter().map(Variable::from).collect();
let outputs = engine.evaluate_many("fraud.json", &inputs).await;
for (row, output) in outputs.iter().enumerate() {
    match output {
        Ok(value) => write_row(row, value.to_value()),
        Err(error) => write_error(row, error.to_string()),
    }
}
```

`evaluate_many` / `evaluate_batch` take the compiled plan (graphs: shredded into columns and run
on the lane VM; policies: the batch policy evaluator), one result per input, in order. Anything the
plan cannot run (a refused node, `trace: true`, an uncompiled policy) falls back to row-by-row
evaluation automatically, with identical results. Chunk large jobs (10k-100k rows per call).

`evaluate_columns` is the fastest path when the input is already columnar (Arrow-like buffers):

```rust
use zen_engine::{ColumnStore, EvaluationOptions};
use zen_expression::lane::{Column, Columns, Values};

let columns = Columns::new(amounts.len())
    .column("txn.amount", Column::new(Values::F64(&amounts)))
    .column("txn.channel", Column::new(Values::Strs(&channels)));
let store = ColumnStore::new();
let output = engine.evaluate_columns("fraud.json", &columns, &store, EvaluationOptions::default()).await;
let first: Variable = output.row(0);
let failed = output.errors[0].is_some();
```

Column paths are dotted input paths. Nulls: `Column::with_validity(values, &bits, offset)` (bit set
= present). Other value kinds: `I64`, `Dec`, `Bool`, `Utf8`/`LargeUtf8` (Arrow offsets + bytes),
`Dict`, `List`, `Struct`, `Any(&[Variable])`.

## (c) One expression per row

```rust
impl BatchExpression {
    pub fn standard(source: &str) -> Result<Self, IsolateError>;
    pub fn unary(source: &str) -> Result<Self, IsolateError>;
    pub fn is_lane(&self) -> bool;
    pub fn evaluate(&self, inputs: &[Variable]) -> Vec<Result<Variable, IsolateError>>;
    pub fn evaluate_scopes(&self, scopes: &[Scope]) -> Vec<Result<Variable, IsolateError>>;
    pub fn evaluate_columns(&self, columns: &Columns) -> Vec<Result<Variable, IsolateError>>;
}
pub fn evaluate_expression_batch(source: &str, inputs: &[Variable])
    -> Result<Vec<Result<Variable, IsolateError>>, IsolateError>;
```

```rust
use zen_expression::{BatchExpression, Variable};

let remainder = BatchExpression::standard("amount / (avg_ticket ?? 1) > 3 and channel != 'atm'")?;
let rows: Vec<Variable> = frame_rows.iter().map(|r| Variable::from(r.clone())).collect();
let values = remainder.evaluate(&rows);
let column: Vec<serde_json::Value> = values
    .into_iter()
    .map(|r| r.map(|v| v.to_value()).unwrap_or(serde_json::Value::Null))
    .collect();
```

Compile once, evaluate many times (the struct is `Clone`; keep one per feature column). The
lane VM is used when it accepts the expression; otherwise the stack VM runs it per row (same
results, `is_lane()` tells which). A syntax error fails `standard()`; runtime errors are per row.

## (d) Host functions

```rust
pub struct HostFunction {
    pub name: Arc<str>,
    pub description: String,
    pub definition: Arc<dyn Fn() -> Rc<dyn FunctionDefinition> + Send + Sync>,
}
pub fn register_host_function(function: HostFunction) -> anyhow::Result<()>;
```

```rust
use std::{rc::Rc, sync::Arc};
use zen_expression::functions::*;
use zen_expression::variable::{Variable, VariableType};

register_host_function(HostFunction {
    name: Arc::from("lookup"),
    description: "One record of a dataset, latest version as of this row".into(),
    definition: Arc::new(|| Rc::new(StaticFunction {
        signature: FunctionSignature { parameters: vec![VariableType::String, VariableType::Any], return_type: VariableType::Any },
        implementation: Rc::new(|args: Arguments| Ok(fetch(args.str(0)?, args.var(1)?))),
    })),
})?;
```

- Process-wide; call before compiling anything that uses the name. Built-in names are refused
  (`duration`, `date`, `sum`, ... ). Re-registering replaces it on threads that have not used it.
- Variadic functions: `CompositeFunction { signatures, implementation }` with one signature per
  arity (e.g. 1..=6 parameters of `VariableType::Any`); `args.0.len()` is the call's arity.
- Parsed, type-checked, completed (intellisense), and evaluated by the stack VM, the lane VM and
  compiled graphs. A host function can return a list consumed by aggregates:
  `sum(same(card, '7d') as t, t.amount)`.
- Keep implementations deterministic in their arguments: the lane VM may share a call between
  rows with equal arguments (calls are never constant-folded at compile time).

## Aggregates

`count sum avg min max median mode stddev variance percentile topK lastN first last unique
countDistinct count_distinct argMax argMin skew kurtosis countDistinctApprox percentileApprox`
take a plain list (`stddev(xs)`) or a list with a projection and an optional filter
(`stddev(list as t, t.amount[, t.kind == 'in'])`, or `#` instead of `as t`). Parameterized ones
put the parameter after the projection: `percentile(list as t, t.amount, 0.95[, filter])`,
`topK(list as t, t.merchant, 3)`, `lastN(list as t, t.amount, 5)`. The callback forms skip null
projections. Plain lists: `mode stddev variance percentile topK lastN first last countDistinct`
skip nulls too, but `sum avg min max median` fail on a null (legacy Zen), so write
`sum(xs as t, t)`. `count(xs)` counts every item, nulls included; `count(list as t, cond)` counts
rows where `cond` is true. `mode` is the most frequent non-null value (ties: the largest),
`first`/`last` the first/last non-null in list order. Empty input: `sum`, `count`,
`countDistinct` are `0`, `unique` is `[]`, the others `null`.

`duration('1d')` is a built-in that returns seconds (86400), not milliseconds, and its name cannot
be taken by a host function: use `duration('1d') * 1000` or a host function with another name.

## Numbers (100k rows, M-series laptop under load, release)

`cargo run --release -p zen-engine --example batch_rows`

| path | rows/s |
| --- | --- |
| `evaluate_many`, 4-expression graph, row inputs | ~0.4-0.95M |
| `evaluate` per row, same graph | ~0.1-0.6M |
| `evaluate_columns`, same graph, columnar input | ~4M |
| `BatchExpression::evaluate`, row inputs | ~1-4M (about the stack VM per row) |
| `BatchExpression::evaluate_columns` | ~9-60M |
