# Lane VM

Branch: `feat/lane-vm` (fresh from `master` at `adf7cdf4`). Started 2026-09-23.

This document records why we are doing this, what we measured, what we decided, and
what is still open. Update the decision log and progress log as work lands.

## Goal

One execution backend for ZEN, inside `zen-expression`, that runs a compiled program over
**lanes** (1..=64 rows at a time) with typed registers where types are known and a
generic `Variable` path where they are not. The engine compiler is built on top of it
afterwards.

Targets carried over from the previous effort: bulk/sink/columnar throughput first
(future big-data / PySpark integration), warm single evaluation must not regress,
behaviour identical to today (errors, traces, `$nodes`, policies).

## Why not continue `feat/decision-compiler`

The previous branch built a compiler with its own IR (~35.6k lines in
`core/engine/src/compiler/`) that **re-implements expression semantics inside the engine**:
typed `DecExpr`/`U32Expr`/`DynExpr` arenas, a separate lowering layer, hosted Dyn ops, and
many representation-bridging steps (`Env`, `EnvKey`, `Carry`, `Pick`, `Merged`,
`Materialize`). Fast paths were added as special cases with admission rules
(relationship kernel, object-array adapter, projection).

Against its own measures of done it stalled: hand sink median 64× (target ≥100×), 27 of
86 hand graphs below 30×, object batch unchanged. Root cause assessment:

- The IR type system has scalars + `Dyn` only (`SlotTy = Dec | Bool | Enum | Str | Dyn`).
  Every object, array, `$nodes` value and loop item is `Dyn`, which forces the bridging
  steps and the special cases.
- Two engines with one semantics (walker + compiler) caused ~5.3k lines of duplicated
  logic and recurring drift.
- In object mode the remaining cost is at the edges (typed object batch profile:
  marshal 48%, output assembly 25%, lane compute 4%), which the IR did not address.

The execution model (lanes, masks, kernels, tiers, fallback) was right; the placement was
wrong. The lane VM keeps the model and moves it into the expression VM.

## Evidence

### Where the interpreter's time goes (Sep 23 measurements)

Existing stack VM, warm, bytecode cached:

| expression | ns/call |
|---|---:|
| `10` | 42 |
| `saleAmount` | 31 |
| `saleAmount * conversionRate + 5` | 71 |
| ternary, 13 ops | 79 |
| unary cell `> 1000` | 36 |
| template `${a} ${b}` | 256 |
| `sum(map(items, #.amount))` | 239 |
| JSON → `Variable` (input object) | 757 |

~30-40 ns fixed per call (scope clone, name lookups, refcounts), ~5-8 ns per opcode. A
median hand graph evaluates ~20 expressions (~1-1.4 µs of VM) out of ~6.6 µs; the rest is
walker orchestration and `Variable` work. No row-at-a-time interpreter reaches 100×
(≈0.66 ns per operation budget); only dispatch amortised across lanes plus typed data does.

`rust_decimal` vs f64 on 64k slices: add/mul ≈ 2 ns (same as f64), compare 3.9 ns,
**div 26 ns** (15× f64). Decimal is not the ceiling except division.

### Lane VM spike (Sep 23)

Scratch crate, now in `.temp/lane-vm/spike/` (gitignored). ~2.3k lines of VM + harness.

- zen-expression parser → flat register program; 64-lane registers (Decimal slots,
  `u64` bools, borrowed/owned strings, `Variable`), validity mask per register, runtime
  lane masks for `and`/`or`/ternary/`??`.
- Exactness: a lane the typed path cannot handle sets `bail` and reruns on the existing
  row VM. Unsupported **subtrees** run through the existing VM per lane (source span
  compiled separately); input-free subtrees (`date('now')`) run once per batch.
- Object literals are compile-time records of registers (no allocation). Arrays of
  objects are split into offsets + child columns; closures run as sub-programs over child
  lanes.

Workload: 83 expression nodes from the hand graphs, real node inputs captured from walker
traces, 512 rows each, sink mode for both engines.

| | value |
|---|---|
| rows differing from walker | **0** |
| lane VM median | **27.9 ns/row** |
| current compiler ÷ lane VM | median **1.40×**, geomean **1.61×**; lane VM faster on 60/83 |
| fully typed nodes (75) | median 1.43×, geomean 1.82×, lane median 24.8 ns |
| walker ÷ lane VM | median **84×** (walker ÷ current compiler: 52×) |

Losses: ~8 nodes whose `date()` subtrees run the row VM per lane (300-1600 ns/row); the
current compiler has a native date path there. Aggregate over one row of each node:
lane 10.1 µs vs compiler 9.4 µs, entirely from those nodes.

Not covered by the spike: tables, switches, loop/`inputField` nodes, `$nodes`, traces,
whole graphs, object-mode output.

### E0: vector width and mask density (Sep 23)

75 fully typed spike nodes (no islands, no bails), widths 1-64, fit `t(W) = a + b/W`.

| | value |
|---|---|
| median ns/row at width 1 / 8 / 64 | 90.9 / 33.8 / 25.6 (width 1 → 64: 3.8×) |
| per-chunk overhead share at width 64 | median 4.3%, p90 11.1%, max 20.5% |
| predicted gain of width 256 / 1024 over 64 | median 0.2% / 0.9%, p90 10.2% / 12.3% |
| small nodes (<20 ns/row, 30 of them) | overhead share 7.3%, predicted gain at 1024: 6.0% |

Most of the amortisation happens by width 8-16; wider vectors are not worth a multi-word
mask type. The remaining per-row cost is per-lane work (Decimal ops, loads, string copies),
not dispatch.

Mask density was **not measurable**: 75 of 89 hand graphs ship exactly one test input, so
cycled rows are identical and every branch is all-or-nothing (100% of executed steps
fully active, 1,480 of 8,928 steps skipped). Density, and any speculation experiment,
needs varied inputs (generated corpus cases or perturbed hand inputs).

### E1: one kernel, two drivers (Sep 23)

Arithmetic, comparisons and `abs`/`floor`/`ceil`/`neg`/`round` rewritten as kernels defined
once (enum ids, D17), operands generic over register/constant, and two generic drivers:
typed bank and `Variable` (unwrap → same kernel → wrap). "Generic mode" compiles every read
as a `Variable` row load and every arithmetic/compare through the `Variable` driver.
74 nodes (no whole-expression fallback), results identical to the walker except 3 nodes
with `date('now')` (clock moved between runs).

| | median ns/row |
|---|---:|
| typed, hand-written ops (spike) | 21.7 |
| **typed, generic kernel drivers** | **16.0** (drivers/hand: median 0.87, geomean 0.86) |
| generic `Variable` lanes, width 64 | 137.8 |
| generic `Variable` lanes, width 1 | 199.7 |
| stack VM per row (expression-node emulation) | 416.8 |

60 nodes compiled fully in generic mode without islands; ratios over those:
generic w64 / typed = **7.4×** median (typing is the main lever, not lanes alone);
generic w1 / stack VM = **0.53** median (the untyped single-row path is already ~2× faster
than today's VM); stack VM / generic w64 = 3.0× median.
Caveat: the stack-VM baseline builds the node's `$`/output object per row, which the lane
sink does not (records stay in registers); object-mode output adds ~50-150 ns/row.

### E2: `TypesProvider` as the type source (Sep 23)

Input `VariableType` built from the column kinds (`Nullable` where a field is ever
missing), `$` fed forward from each expression's result type, `TypesProvider::generate`
(non-strict) over every expression of the 83 nodes.

| | value |
|---|---|
| expressions | 282 |
| non-literal AST nodes with a concrete type | **99.2%** (scalar: 67.8%, the rest objects/arrays) |
| result typed: both / TypesProvider only / spike only / neither | 252 / 29 / 1 / 0 |
| soundness: row outputs not fitting the predicted type | **0 of 18,048** |

`TypesProvider` types more than the spike's own inference (the 29 include `date()`,
`values()` and similar that the spike sent to islands). The signature here equals what
speculation would produce after warm-up on these inputs.

### E3: one quickened program, dual-representation registers (Sep 23)

Registers carry a typed bank plus a `boxed` mask and overflow `Variable` slots. `LoadRow`
loads typed values from row objects with the type chosen by majority vote per field;
values that do not fit are boxed in place. Arithmetic, comparisons, equality and set tests
run boxed lanes through the same semantics (`ops.rs`) inside the instruction; errors are
final at the instruction. Other instructions bail boxed lanes (conservative).
621 runs, **0 results differing from the walker**.

| clean data, 75 nodes | median ns/row |
|---|---:|
| typed drivers, `Columns` input (E1) | 21.0 |
| quickened, row input, boxed checks off | 129.7 |
| quickened, row input, boxed checks on | 131.3 (overhead 0.7%) |

| one input field corrupted in p% of rows, 66 nodes | box in place ÷ clean | bail + rerun ÷ clean | bail ÷ box |
|---|---:|---:|---:|
| 1% | 1.00 | 1.02 | 1.02 |
| 10% | 1.01 | 1.14 | 1.11 |
| 30% | 1.04 | 1.40 | 1.28 |

Row input is 6× slower than column input: the spike walks each path per lane with no slot
caches or shapes, which again puts the cost at the edges.

Same check on the fast path (typed drivers, `Columns` input, 75 nodes, two runs): boxed
checks cost **1.1% median** (p90 2.5%, +0.2 ns/row) over 21.1 ns/row. Quickened typed
instructions are the typed drivers plus this check; with exact sources (`Columns`,
validated schema) no lane can be boxed, so the check can be compiled out.

### E4: decision-table layout, indexes, cell dedup (Sep 23)

Generated tables (100 / 1,000 / 8,000 rules, 5 columns: string equality, two numeric range
columns, `endsWith`/`startsWith`, number equality; default rule last), "broad" (30-70%
empty cells, early first hit) and "specific" (5-50% empty, late first hit), first and
collect, 1,024 random rows. All 12 configurations match the walker (256 rows each).
Code in `.temp/lane-vm/tablebench/`.

ns/row, width 64 (width 1 in brackets where it differs materially):

| rules | profile | policy | first hit at rule | distinct cells | no dedup, no index | dedup, per-rule masks, eq+range index | **dedup, per-lane bitsets, eq+range index** | same, eq-only index (master) | walker |
|---:|---|---|---:|---:|---:|---:|---:|---:|---:|
| 100 | broad | first | 15 | 123 | 1,958 | 359 (432) | **342** (343) | 924 | 4,913 |
| 100 | broad | collect | - | 123 | 1,792 | 377 (635) | **372** (382) | 980 | 15,993 |
| 1,000 | specific | first | 332 | 757 | 22,533 | 499 (941) | **402** (399) | 5,970 | 34,321 |
| 1,000 | broad | collect | - | 647 | 18,279 | 592 (4,544) | **616** (608) | 6,077 | 135,343 |
| 8,000 | broad | first | 2 | 1,777 | 143,253 | 579 (766) | **356** (363) | 15,155 | 3,224 |
| 8,000 | specific | first | 257 | 1,915 | 178,083 | 621 (1,190) | **401** (405) | 18,068 | 32,910 |
| 8,000 | broad | collect | - | 1,777 | 141,836 | 2,277 (47,401) | **2,723** (2,657) | 57,705 | 1,051,159 |
| 8,000 | specific | collect | - | 1,915 | 177,299 | 1,634 (23,360) | **1,817** (1,804) | 64,117 | 853,111 |

- Cell dedup alone: ~11× at 8k rules (distinct cells are ~5% of cells).
- Range index on top of equality: 3-42× (master indexes equality only).
- Per-lane rule bitsets vs per-rule lane masks: equal or better for first-hit, 10-20% slower
  for collect at width 64, and 3-18× faster at width 1 (per-rule masks pay every rule even
  for one row). First-hit cost is flat (~350-400 ns) from 100 to 8,000 rules.
- Walker times include building the input JSON and `Variable` per row (~0.5-1 µs), so small
  table ratios are overstated; at 8k collect it is ~400×.

### Arrow input conversion (Sep 23)

1M values into a `Decimal` register:

| conversion | ns/value |
|---|---:|
| copy baseline | 0.33 |
| `Int64` → `Decimal` | 0.68 |
| `Decimal128` (scale 2) → `Decimal` | 1.13 |
| `Float64` → `Decimal` via `rust_decimal::from_f64` | 95-179 |
| `Float64` → `Decimal` via shortest decimal form (`ryu` + parse) | 25.1 |

The two `Float64` paths agree on all 1M values; the shortest decimal form is what a JSON
number would produce. Code in `.temp/lane-vm/decbench/`.

| Arrow | lane register | copy |
|---|---|---|
| `Boolean`, validity bitmaps | bool bank / validity (`u64` words) | zero-copy (shift if offset % 64 != 0) |
| `Utf8`, `LargeUtf8`, `Utf8View` | string register (`&str` slices) | zero-copy |
| `Dictionary<i32, Utf8>` | `Dict` kind | zero-copy codes, constants resolved once per batch |
| `Int32`, `Int64` | `Decimal` bank | ~0.7 ns/value inside the load |
| `Decimal128(p, s)` | `Decimal` bank | ~1.1 ns/value inside the load; >28 digits boxed |
| `Float64` | `Decimal` bank | ~25 ns/value |
| `Timestamp`, `Date32` | `Date` bank or numeric seconds | cheap arithmetic |
| `Struct` | field paths | zero-copy |
| `List`, `LargeList` | offsets + child registers | zero-copy |
| `Map`, `Union`, other | `Variable` bank | slow path |

### Dates (Sep 23)

Stack VM, warm, bytecode cached:

| call | ns |
|---|---:|
| field read (VM baseline) | 22 |
| `date(rfc3339)` (deprecated, returns unix seconds) | 183 |
| `date('YYYY-MM-DD')` / `date('YYYY-MM-DD HH:MM:SS')` | 178 / 241 |
| `date('now')` | 69 |
| `date(a) - date(b)` | 371 |
| `d(rfc3339)` / `d('YYYY-MM-DD')` | 87 / 296 |
| `d(a).year()` / `.add(1, 'd')` / `.format('%Y')` | 99 / 204 / 180 |
| `d(a).diff(d(b), 'day')` / `.isBefore(d(b))` | 408 / 401 |
| parse only: chrono chain (`helpers::date_time`, up to 3 formats) | 149 per string |
| parse only: hand ISO fast path | 5.6 per string (same results on all 3 formats) |

The spike's losing nodes (300-1600 ns/row) were per-lane row-VM calls around this parse.

### Key lookup and interning (Sep 23)

Bench in `.temp/lane-vm/keybench/`. All 89 hand-graph inputs, 512 rows each, 9 read paths
per doc (~1.3 segments each), objects average 3.5 top-level keys.

| strategy | ns / path segment |
|---|---:|
| `VariableMap::get` today | ~10 |
| same scan without `Rc`/`RefCell` | 7.8 |
| slot cache per load site, key compare | 6.4 |
| **slot cache, 8-byte fingerprint compare** | **5.2** |
| per-run interning at read (hash row keys) | 12.6 (rejected) |
| id-only entries, per-Isolate interner | 2.6 |

| conversion | ns per corpus row (89 docs) |
|---|---:|
| JSON → `Variable` | 39.6-41.3k |
| JSON → id-only entries (per-Isolate interner) | 32.8-36.0k (~15% cheaper) |
| existing `Variable` → id-only entries | +16-17.7k on top |

Per input row: conversion ~460 ns, key reads 66 ns (slot cache) vs 34 ns (interned), lane
execution ~28 ns. **Edge conversion dominates object mode**, not key lookup.

## Decisions

| # | decision | reason |
|---|---|---|
| D1 | Fresh branch from `master`; harvest from `feat/decision-compiler` instead of continuing it | IR duplicated expression semantics; see above |
| D2 | One backend: a lane VM inside `zen-expression`; the engine compiler comes afterwards and targets it | removes interpreter/compiler drift; one semantics in `ops.rs` + `functions/` |
| D3 | The lane VM has a **generic path for every opcode** (`Variable` bank calling `ops.rs`/functions per lane) before any typed fast path | single backend must be total; typed paths are optimisations |
| D4 | **Fallback contract**: any lane the typed path cannot handle exactly falls back to the generic semantics; results and errors are always the generic path's. Mechanism refined by D24 (box in place instead of rerun) | exactness by construction while fast paths land incrementally |
| D5 | Registers live in a `Frame` with runtime width 1..=64 (`u64` masks); width 1 for `Isolate`, 64 for batches; one executor, no `const W` duplication | shared code for single and batch; wasm code size; E0 confirms 64 is enough |
| D6 | Input reaches lanes only through `Load` ops with pluggable sources (`Row`, `Rows`, later `Columns`, `Records`); no eager conversion of whole `Variable`s | only read paths are paid for |
| D7 | `Variable` public enum unchanged (`Array`, `Symbol` unchanged); it is the boundary type and the generic register bank | public API across engine, bindings, serde |
| D8 | `VariableMap` gets **shapes** (key layout, refcounted, from the previous branch); load sites use a slot cache (fingerprint check first, shape-id check once shapes land) | fast reads and cheap construction of known layouts with plain `Rc` ownership |
| D9 | Shapes carry **keys only**; field and element types live in the VM (feedback, schema, column kinds) | `Variable` is mutable; tracking types on every write would tax all zen-types users |
| D10 | No global, per-run or per-Isolate interning of `Variable` keys | global: DoS and locking; per-run at read: slower (12.6 vs 5.2 ns); per-Isolate: ids cannot outlive the Isolate while `Variable` does, ownership gets messy for ~15% conversion |
| D11 | Arrays get no shape/elements-kind in `Variable`; uniform record arrays detected by shape-pointer equality and split into child lanes; unboxed arrays only as `Column::List` input | V8 elements kinds pay off via unboxed storage, which would change the public enum |
| D12 | String **values** may be dictionary-encoded per batch (`Dict` kind, `u32` codes) for equality/set tests; dictionary comes from input or a hashed builder | old branch lesson: linear memcmp interning cost 81% of `load_chunk` |
| D13 | Old stack VM stays only as test oracle until parity, then is deleted | differential safety net |
| D14 | JIT is out of scope (wasm target) | unchanged from before |
| D15 | Keep width ≤64 with `u64` masks. Selection vectors are revisited with decision tables (stage 3) on varied data | E0: beyond 64, predicted gain is ~1% median (p90 ~12%); per-chunk overhead at 64 is 4.3% median; density could not be measured on the hand corpus |
| D16 | Batch-level adaptivity: cheap per-batch facts (no nulls, single shape, dictionary strings) pick kernel variants | Photon/Velox; same idea as speculation, applied per batch |
| D17 | `Program` is plain serialisable data: kernels referenced by enum ids, no `fn` pointers or closures in the IR; it is `Send + Sync` so worker threads share one compiled program, each with its own frames | enables shipping a program embedded in the wasm runtime (JSON in, JSON out) without codegen; Spark workers share programs |
| D18 | Compile unit can hold many expressions with shared loads; conditions can yield mask registers | engine needs cross-expression CSE, dead-output elimination and column-wise table bitmaps |
| D19 | No fixed-point/`i64` fast path; improve `rust_decimal` (division first, 26 ns) if it becomes the bottleneck | kernels isolate `Decimal`, improvements drop in |
| D20 | Scalar ops are **single-sourced kernels** (enum id) run by generic drivers: typed banks, and `Variable` lanes via unwrap → kernel → wrap; operands generic over register/constant; the stack VM's `ops.rs` becomes the same wrapper | E1: generic drivers 13% faster than hand-written typed ops |
| D21 | Type inference for typed programs reuses IntelliSense `TypesProvider`; every type source (columns, schema, speculation) only produces the input `VariableType` | E2: 99.2% of AST nodes concrete, 0 unsound outputs in 18k checks |
| D22 | Deprecated `date()`/`time()` (return numbers) become typed `Str → Num` kernels with a hand-written ISO fast path (`YYYY-MM-DD`, `YYYY-MM-DD HH:MM:SS`, `…THH:MM:SSZ`); anything else falls through to today's chrono chain; `date('now')` is read once per evaluation | only date forms in the hand corpus (26 `date(`, 8 `time(`); parse is ~149 ns via chrono vs 5.6 ns fast path; results identical by construction; shared with the stack VM through D20 |
| D23 | `d()` gets a typed `Date` bank (`Option<DateTime<Tz>>`, unboxed, `Copy`); methods become kernels (getters Date→Num, comparisons Date×Date→Bool, `diff` Date×Date→Num, `add`/`sub`/`startOf`/`endOf` Date→Date, `format` Date→Str) with literal units/formats as kernel parameters | removes `Rc<dyn DynamicVariable>` and per-call parsing; `TypesProvider` already types `d()` as `Date` |
| D24 | **One quickened program**: instructions come in generic and typed variants of the same kernel; registers carry a typed bank plus a `boxed` mask and overflow `Variable` slots; a lane that does not fit is boxed in place and handled inside each instruction by the same semantics, errors are final at the instruction. Replaces the "rerun the generic program" part of D4 and the typed-program-per-signature cache | E3: 0.7% overhead on clean data; degradation 1.00/1.01/1.04× at 1/10/30% corrupted rows vs 1.02/1.14/1.40× for bail-and-rerun |
| D25 | Types come from static sources first (`TypesProvider` over columns/schema) and from speculation on the **input signature** (majority vote per load site, once after warm-up, into a per-runner copy, then frozen); per-instruction re-quickening only as a safety valve for fields whose boxed rate climbs | static types cost ~100-200 lines on top of an existing type system; runtime-only quickening needs counters on every instruction, bank changes after allocation, and locking for shared programs (p99 spikes) |
| D26 | Outputs are built from shapes: statically known key sets (object literals, expression-node keys, table outputs) get a compile-time `Shape`; dynamic outputs (`merge`, pass-through merges, function results) use an inline cache per output site (input shape + keys → output shape) | output assembly was 25% of typed object-batch time; known-shape construction is allocate + fill, no key hashing or duplicate checks |
| D27 | The primary compile unit is a **set of named expressions** (an expression node, a table column, eventually a graph) compiled into one program with shared loads; `Isolate`'s single-expression API is a thin wrapper | per-call overhead dominates small expressions (stack VM: 30-40 ns fixed vs ~5 ns of work for `x == 'a'`); `customer` is resolved once per lane for all its fields |
| D28 | Register allocation reuses dead registers: frame size follows peak live values, not total values | spike allocated a fresh register per value; an 8k-rule table would need ~24 MB per width-64 frame |
| D29 | Decision-table columns are a VM op ("cell set"): unary cells over one value, identical cells deduplicated, equality cells hashed to rule bitsets, range cells on sorted boundaries (region → rule bitset), other cells as small programs over the distinct cells. Result layout is **per-lane rule bitsets**; the engine ANDs columns word by word (early exit at the first non-zero word for first-hit) and applies hit policies | E4: dedup ~11×, range index 3-42×, per-lane bitsets equal/better at width 64 for first-hit and 3-18× better at width 1; first-hit flat ~350-400 ns from 100 to 8k rules; master's `nodes/decision_table/index.rs` indexes equality only |
| D30 | Programs accept an initial lane mask and can produce masks as results | tables, switches and first-hit rules shrink the active row set as they go |
| D31 | `Input::Columns` mirrors Arrow's physical layout as borrowed views (bit-packed booleans/validity with offset, offsets + data and 16-byte views for strings, dictionary codes, raw `i64`/`i128`/`f64` buffers with a type tag, list offsets + child views); no arrow-rs dependency in zen-expression, bindings map arrow-rs buffers onto the views; numeric conversion happens inside the load per chunk; `Float64` converts via the shortest decimal form (matches JSON) | zero-copy for booleans, validity, strings, dictionaries, structs, lists; ~1 ns/value for `Int64`/`Decimal128`; `Float64` ~25 ns/value, paid once per field read (D27); the old branch's `Column` used `&[bool]`/`&[&str]`, which force copies |
| D32 | Closures (`map`, `filter`, `flatMap`, `some`, `all`, `none`, `one`, `count`, aggregates) run as **element-parallel child lanes** for both generic and typed code: the elements of all rows in a chunk form one run of child lanes with offsets back to their parent; the body is a sub-program. Chains of list operations are fused until something materialises (output, `len`, indexing); `flatMap` yields nested offsets. Short-circuit and error order are preserved: child lanes record errors without failing, each parent resolves them in element order and an error counts only before the result is decided. Aliases work like `#`; nested closures are further offsets layers; outer values and `$` are gathered by parent index; mapped records stay as child registers until output (D26) | per-row loops over elements lose lane amortisation; fusion removes intermediate arrays (the spike's `ListAgg`); element-parallel evaluation would otherwise raise errors that `some`/`all`/`one` never reach today |
| D33 | Compiled programs hold no `Variable` or `Symbol`: constants are plain data (`Arc<str>`, `Decimal`, `bool`, nested constant trees); runners materialise `Variable`s lazily; output shape templates (D26) and speculation feedback live per runner | `Variable` contains `Rc` and `Symbol` is `hipstr::LocalHipStr`, neither is `Send`, which D17 requires; today's `Opcode` already uses `Arc<str>` |
| D34 | The frame keeps each lane's first `VMError` next to the error mask; the generic path calls `ops.rs`, so messages match today's; traces (policy `enhance_trace` operands, table traces) come from a separate compile mode that emits recording ops, normal programs carry no tracing | error parity per lane; no tracing overhead in production programs |
| D35 | Parity: the stack VM stays only as an oracle (differential runs at widths 1 and 64, engine corpus through the walker, a random-expression fuzzer near the end with failing seeds pinned). Before deleting it, its answers (corpus, test inputs, pinned fuzz seeds, exact errors) are recorded as golden files; intended differences (e.g. `date('now')` once per evaluation, D22) go in an explicit allowlist | parity stays checkable after deletion; behaviour changes show up as reviewed diffs |

## Design (current)

```rust
pub struct Program {
    code: Vec<Step>,
    slots: SlotCounts,
    masks: u16,
}

pub struct Step {
    mask: u16,
    op: Op,
}

pub struct Frame {
    width: usize,
    dyn_: Vec<Variable>,
    num: Vec<Decimal>,
    bool: Vec<u64>,
    str: Vec<Text>,
    valid: Vec<u64>,
    masks: Vec<u64>,
    bail: u64,
}

pub enum Input<'a> {
    Row(&'a Variable),
    Rows(&'a [Variable]),
    Columns(&'a ColumnBatch<'a>),
    Records(&'a RecordBatch),
}
```

- Module: `core/expression/src/lane/` (`program.rs`, `compiler.rs`, `exec.rs`, `frame.rs`).
- Register `r` of a bank occupies `bank[r * width .. (r + 1) * width]`.
- Stage 1 has only the `Variable` bank (the "VariableLane"); typed banks come later.
- Frames are pooled: `Isolate` keeps a width-1 frame, batch runners a width-64 frame.
- Compiled `Program` is immutable and shareable (`Arc`); feedback counters and typed
  program caches live per Isolate/runner (per thread).

### Where performance comes from

| source | removes |
|---|---|
| unboxed typed registers | per-op enum match, refcount clone/drop |
| lanes × types | one dispatch per op per 64 rows with tight loops |
| bool as `u64` masks | branches and logic as single word ops |
| constant specialisation | `x > 1000`, `tier == 'gold'` as compare-with-constant ops |
| validity bitmaps | per-value null checks |
| strings without allocation | borrowed `&str`, dictionary codes |
| records as registers | allocation of intermediate objects; output built once from a known shape |
| typed kernels | shared implementations in `functions/kernels.rs` |
| shapes | field lookup at input, object construction at output |

Shapes speed up the edges; typing speeds up everything between them.

### Typed stage (current design)

- One program (D24). Instructions exist in generic and typed variants of the same kernel
  (D20); registers carry a typed bank, a validity mask, a `boxed` mask and overflow
  `Variable` slots. A lane that does not fit is boxed in place and handled inside each
  instruction with the generic semantics; errors are final at the instruction.
- Initial variants come from `TypesProvider` over the input `VariableType` (D21): exact for
  `Columns` and validated schemas. Without those, load sites vote on kinds for a warm-up
  window and the program is re-specialised once into a per-runner copy, then frozen (D25).
  Re-quickening a single instruction is a safety valve for rising boxed rates.
- Kinds beyond scalars: `Rec(shape)` (compile-time field → register map), `List(elem)`
  (offsets + child registers, closures as sub-programs), `Dict`, `Date` (D23).
- Outputs are built from static or cached shapes (D26).
- Order: `Columns` first (exact types, no speculation), schema-typed rows, then input
  speculation. Speculation thresholds need a varied-input corpus.

### Borrowed ideas (Sep 23 review of V8, TurboFan, DuckDB, Velox, Photon, CPython, LuaJIT)

- V8 maps onto the plan: feedback vectors ≈ load-site feedback, hidden classes + ICs ≈
  shapes + slot caches, speculation/deopt ≈ quickened instructions + boxed lanes (D24), escape analysis ≈
  records as registers, inlining ≈ whole-graph programs. Its remaining edge is machine
  code, which lanes replace by amortising dispatch.
- Adopted: batch-level adaptivity (D16), width kept at 64 after E0 (D15),
  serialisable programs (D17), engine-level table bitmaps and cross-node analysis (D18).
- Not adopted: tracing JIT (no hot loops in rules), assembly/threaded interpreters (wasm),
  NaN-boxing (`Decimal`), fixed-point tier (D19).

## Open questions / to probe

**Decisions deferred during the autonomous build (need Stefan):**

- A. Make the lane VM the default `Isolate` backend? Evidence: engine suite 410/410 on
  both backends, ~20M+ fuzz comparisons clean, graphs 1.15× faster with none slower,
  single-expression width 1 at 1.02×. Currently behind feature `lane` (D13 keeps the
  stack VM as oracle until parity; parity looks reached).

1. Typed stage: design settled (D20-D26); open are speculation thresholds (warm-up length,
   vote threshold, boxed-rate safety valve), which need a varied-input corpus.
2. Error and trace parity per lane: exact message and failure point; policies'
   `enhance_trace` operand values.
3. Hard constructs in the generic lane VM: assignments, `$root`, methods, object mutation
   and `Rc` aliasing semantics (closures, aliases and nesting are designed in D32).
4. Single-evaluation cost: width-1 path vs today's `Isolate` (~40 ns fixed per call). Cold
   compile cost is not a concern (Stefan, Sep 23).
5. Custom/extension functions registered at runtime must work per lane.
6. Dates: decided in D22/D23. Still open: time-zone semantics (per-call `tz` vs global
   default) and invalid dates in the `Date` bank (validity bit plus an "invalid date"
   state to match today's `Invalid date` output). The ISO fast path needs a randomized
   differential test against the chrono chain.
7. Shape transition-tree growth on untrusted inputs (thread-local root, fan-out caps).
8. Engine stage: tables, hit policies, `$nodes`, pass-through merges, walker order.
9. Division by constants 2ᵃ·5ᵇ as exact multiplication.
10. E0-E4 done (see Evidence).
11. Measure lanes alone (generic bank) vs lanes + types on the spike's 83 nodes.
12. Binding-side input straight to `Records`/`Columns` (the largest object-mode lever);
    per-Isolate interning would only be reconsidered there, capped.

## Plan

| stage | scope | gate |
|---|---|---|
| 0 | Start fresh (no harvest; the old branch is reference only, decision Sep 23) | - |
| 1 | Generic lane VM: register compiler + executor for the full opcode set, `Row`/`Rows` inputs, slot caches | zen-expression suites at width 1 and 64; differential vs stack VM; single-eval benchmark not slower |
| 2 | Typed banks + `Columns` input (Arrow-shaped views, D31), then schema-typed rows, then input speculation (D24-D26) | spike numbers reproduced; lanes-only vs typed measured |
| 2b | Cell-set op in the VM (D29): unary cells, dedup, equality + range indexes, per-row rule bitsets | parity vs stack-VM unary evaluation on E4's generated tables; E4 numbers reproduced |
| 3 | Engine on the lane VM: expression nodes, tables (consume cell sets: AND columns, hit policies, outputs), switches, loops, `$nodes`, traces, graph schedule | walker as oracle; corpus differential and fuzz |
| 4 | Policies | Driver as oracle |
| 4b | Random-expression fuzzer (both VMs, widths 1 and 64), golden files from the stack VM and walker, allowlist of intended differences (D35) | fuzz clean; golden files committed |
| 5 | Delete stack VM and walker; public bytecode API (`Opcode`, `OpcodeCache`, `Expression::bytecode()`) removed as a breaking change | golden files and suites green |

Every opcode gets a generic lane form in stage 1 (lowering is total). Typed forms (D20) are
added by measured value: the corpus op census (field reads, arithmetic, comparisons,
ternaries, `??`, string equality, templates, `in`, `len`, `contains`, dates, object
literals) and profiles, not all at once.

### Reference list from the old branch (read, do not copy)

- zen-types: `variable/shape.rs`, shaped `variable/map.rs` (`insert_new`, `insert_hinted`),
  related `mod.rs`/serializer changes.
- zen-expression: `functions/kernels.rs` (typed kernels), `vm/ops.rs` (semantics pulled out
  of the VM loop), `vm/date/format.rs` (date format caching), opcode cache changes, their
  tests.
- Prerequisite fixes tracked in the old branch's `TODO.md` (decimal serialization,
  interpreter fixes).
- Test infrastructure: differential tests, fuzz harnesses, frozen expected answers
  (`test-data/expected`), `tools/oracle`, corpus/census harnesses.
- mimalloc in the nodejs/python/uniffi bindings.

## Engine on the lane VM (stages 3–4, planned 2026-09-30)

Scope decided with Stefan: graphs **and** policies in the first milestone, batch and
columnar first (single evaluation is a width-1 batch and must not regress), **transparent
replacement** (existing `Decision::evaluate` / `DecisionEngine::evaluate` use the compiled
program when one exists, the walker/Driver otherwise), work continues on `feat/lane-vm`.

### Principles (lessons from `feat/decision-compiler`)

| # | decision | reason |
|---|---|---|
| E1 | The engine layer only **schedules and wires**. Every expression, table cell, switch condition, `inputField` and policy block value is a lane program; merge, `$nodes`, schema validation and date conversion call the walker's own functions (moved to shared helpers, never copied) | the old IR re-implemented semantics and drifted (4 hand-copied predicates, 3 verified drift fixes) |
| E2 | Compilation happens where precompilation already happens: `GraphContent::compile` / `Decision::compile` / `DecisionEngine::compile` (`CompiledSet`). Uncompiled content keeps using the walker. The compiled program sits next to today's `OpcodeCache`/`dt_indexes` in the content | transparent for users who already precompile; no cold-path compile cost (8k-table compile was 81 ms on the old branch) |
| E3 | Anything not yet compiled makes the **whole decision** use the walker (function/custom nodes, trace mode, unsupported shapes), recorded as a compile verdict with a reason. Node-level hosting comes later (stage 3d) | exact by construction; a census of verdicts tracks coverage |
| E4 | Node boundaries start as `Variable` values per lane (stage 3a); typed register flow across nodes (records as registers, D26 shapes, column input) is stage 3b, added only where a node's output shape is static | 3a is exact and simple; the old branch's speed came from 3b-style flow, its complexity from doing it first |
| E5 | Switch pruning becomes **lane masks** (D30): each node runs under the OR of its incoming edge masks; a node whose mask is empty is skipped; walker order (LIFO DFS from input, restart on prune) is reproduced statically for everything observable (merge order, `$nodes` contents, first Output reached) | switches were where the old branch needed "regions"; masks keep one program per graph |
| E6 | Decision tables use `CellSet` per input column (D29) + one `compile_many` program for output cells; hit policies First / Collect / First+collect columns reproduce `evaluate_row` exactly, including "output-cell error = row does not match" | cells are unary programs today; `CellSet` already matches the old VM |
| E7 | New batch APIs: `Decision::evaluate_batch(&[Variable])`, `Decision::evaluate_columns(&Columns)` (+ `DecisionEngine` equivalents); single `evaluate` is a width-1 batch | batch/columnar is the target; there is no batch API today |
| E8 | Parity gate before any node kind is switched on: differential walker-vs-compiled over `test-data/graphs` (89 files, 131 cases), `test-data/*.json`, the engine test suites run in both modes, a random graph fuzzer (new, seeds advance in CI), and the same for policies against the Driver | transparent replacement ships behaviour differences immediately |

### Stages

| stage | scope | gate |
|---|---|---|
| 3.0 | Harness: differential runner (walker vs compiled, row and batch), compile-verdict census over all fixtures, batch benchmark corpus (fixture inputs × generated variants, rows and columns) | runs on master behaviour |
| 3a | Graph program with `Variable` boundaries: input/output nodes (validation, dates), expression nodes (`compile_many` chain, `$nodes`), decision tables (E6, index-free first, `TableIndex` as a lane mask prefilter later), switches (E5), transform attributes (`inputField`, `outputPath`, `passThrough`, loop mode as child lanes), sub-decisions (compiled child program called per lane, depth limit), errors (first failing node per lane, `NodeError` shape) | all fixtures + engine suites identical in both modes; fuzz clean; single evaluation not slower |
| 3b | Typed flow: column input bound straight to registers, statically shaped node outputs kept as registers, downstream reads resolved at compile time, output assembled once from shapes; `evaluate_columns` returns `Output` columns | 3a gates + batch/columns benchmark targets (set after 3a numbers) |
| 4 | Policies: `EvalArtifact` + execution order compiled into one program; blocks as lane programs, match/table selection as masks, iterated blocks as child lanes, `hydrate_references` and store writes via existing helpers | Driver differential over policy suites, fixtures and a policy fuzzer |
| 3d | Function/custom nodes as host steps (per-lane calls into the existing handlers), traces in compiled mode (recording ops, D34) | walker parity incl. traces |
| 5 | Make compiled the only path once parity holds for everything; walker/Driver become test oracles | golden files (D35) |

## Progress log

- 2026-09-23 — Architecture review of `feat/decision-compiler`; lane VM spike (83 nodes,
  0 mismatches, 1.40× median over the current compiler); key lookup/interning benchmark;
  decisions D1-D14 recorded; branch `feat/lane-vm` created from `master`.
- 2026-09-23 — Review of V8/TurboFan/DuckDB/Velox/Photon ideas (D15-D19). E0: width stays
  ≤64 with `u64` masks; hand corpus too uniform to measure branch density.
- 2026-09-23 — E1: single-sourced kernels with generic drivers are faster than hand-written
  ops; generic `Variable` lanes at width 1 are ~2× faster than the stack VM; typed is 7.4×
  faster than generic. E2: `TypesProvider` types 99.2% of nodes with no unsound results.
  D20, D21 recorded.
- 2026-09-23 — E3: dual-representation registers with in-instruction fallback (0.7%
  overhead, flat degradation); D24-D26 recorded (one quickened program, static +
  input-speculated types, shaped outputs).
- 2026-09-23 — Pre-build review: D33 (no `Variable`/`Symbol` in programs), D34 (per-lane
  errors, trace compile mode), D35 (parity strategy, golden files, allowlist). Cold compile
  cost not a concern; fuzzer near the end; breaking the public bytecode API is fine.
- 2026-09-23 — D32 (closures as element-parallel child lanes, fused chains, order-preserving
  short-circuit and errors); plan note on prioritising typed forms by census and profiles.
- 2026-09-23 — Plan: cell-set op moved into the VM as stage 2b; indexes live with unary
  semantics in zen-expression, the engine consumes per-row rule bitsets.
- 2026-09-23 — Arrow/PyO3 input path: conversion costs measured; D31 recorded, D17 extended
  with `Send + Sync`.
- 2026-09-23 — E4: per-lane rule bitsets + cell dedup + equality/range indexes; D29 settled.
- 2026-09-23 — Many-small-expressions review: D27-D30 (multi-expression programs, register
  reuse, cell sets, masks in/out). `test-data/8k.json` found to be degenerate (2 distinct
  cells per column), so E4 needs generated tables.
- 2026-09-23 — Date costs measured; D22 (deprecated `date()`/`time()` as `Str → Num` kernels
  with ISO fast path, `date('now')` once per evaluation) and D23 (typed `Date` bank for
  `d()`) recorded.
- 2026-09-23 — Build started (autonomous session). `vm/ops.rs`: all stack-VM semantics as
  pure functions with identical errors; `vm.rs` now calls them (D20 single source).
  `lane/`: register program (`program.rs`), AST compiler with register reuse and masks
  (`compile.rs`), executor with per-lane errors, masks, child-lane closures and
  env/assignment support (`exec.rs`), public `LaneProgram`/`LaneRunner` (`mod.rs`).
  `tests/lane.rs`: differential vs the stack VM over standard/date/unary CSVs (each case,
  plus every expression × every input of its file as width-64 batches) and a special-case
  set (closures, aliases, nested closures, assignments, `$root`, templates, intervals,
  overflow): **62,684 comparisons, 0 differences**; a deliberately broken op is caught
  (1,636 differences).
- 2026-09-23 — Performance pass on the lane VM (all changes kept under the differential
  gate: 185,958 comparisons incl. specialized and columnar programs, 0 differences):
  - typed registers with boxed lanes (D24): `Num` (Decimal bank) and `Bool` (`u64` bits)
    registers, boxed mask + `Variable` overflow; generic ops convert on read/write so every
    op stays correct; typed fast paths for arithmetic, comparisons, `not`, branches,
    merges and equality with constants; structural kinds (arithmetic → Num, comparisons →
    Bool) apply without any input types;
  - speculation (D25): load sites record hint keys (`a.b`, `items[]`, `items[].x`); a
    majority vote over sample rows recompiles with kinds; `specialize_columns` takes kinds
    from column types;
  - slot caches per load site (`VariableMap::get_hinted`, index hint + key check), pinned
    registers for closure elements/imports, direct field op for `#.x`, element kinds for
    closures, short-circuit closures in growing rounds, constant arrays for `in`;
  - fused `load == const` (`LoadEq`): compares in place against the column or through
    the slot cache, no `Variable` built;
  - column input (D31): `lane/columns.rs` (Arrow-shaped borrowed views: Decimal/i64/f64,
    bit-packed bools and validity, Utf8/LargeUtf8 offsets+data, `&str` slices, `Variable`
    fallback); load sites bind to a column, to "absent", or to row materialization only
    when a column cannot answer (object reads, `$root`, assignments).
  Bench (`benches/lane.rs`, 185 corpus expressions, 512 rows, vs stack VM with cached
  bytecode): width 1 0.95× geomean (median 53.9 vs 62.7 ns); width 64 rows 2.04×; columns
  3.17× geomean, median 12.7 ns/row; typed numeric columns 8-12× (6-10 ns/row).
- 2026-09-23 — Parity fuzzing (`tests/lane.rs`): two generators — type-messy random
  expressions (arithmetic, logic, `??`, ternaries, `in`/intervals, templates, member/index
  access, slices, ~20 builtins, all closure kinds with aliases, assignments, literals)
  over inputs whose fields are sometimes numbers/strings/null/missing/objects, and a typed
  generator (numeric/boolean/string expressions over mostly clean inputs with occasional
  nulls and wrong types). Every expression runs through generic width 1 and 64,
  speculated programs, and columnar input (generic and column-specialized), compared with
  the stack VM on values and exact errors. One real finding, fixed: a path that is both a
  column and a prefix of other columns must bind to the row path. After the fix: ~20.8M
  comparisons over 9 seeds, 0 differences (value/error mix ~40/60 for the messy
  generator, ~85/15 for the typed one). Defaults run 300 cases per generator in `cargo
  test`; `LANE_FUZZ_ITERS`/`LANE_FUZZ_SEED` scale it.
- 2026-09-23 — Engine parity: `zen-expression` feature `lane` (forwarded by `zen-engine`
  feature `lane`) routes `Isolate::run_standard`/`run_unary` through the lane VM at width
  1 (`lane::Backend`, thread-local program cache). The engine never calls the bytecode
  API directly, so this puts every graph, decision table, expression node, switch and
  policy expression on the lane VM. `cargo test -p zen-engine --features
  "arbitrary_precision lane"`: **410 passed, 0 failed** — identical to the stack-VM
  baseline (410/0). The default backend is still the stack VM (see open decisions).
- 2026-09-23 — Engine-level measurement (86 hand graphs without function nodes, test
  inputs, `Decision::evaluate`, stack VM build vs `lane` build): stack/lane median
  **1.15×**, geomean 1.15×, one pass over all graphs 654.6 µs → 570.8 µs; no graph
  slower (worst 1.00×). The walker's own overhead still dominates; the lane VM's batch and
  columnar wins need engine integration (stage 3). Fused `LoadCmp` (`field <op> number`)
  and `SelectConst` (`cond ? 'a' : 'b'`), per-frame constant pool: expression bench width
  1 now 1.02× geomean (total 14.2 µs vs 14.7 µs stack), width 64 1.94×, columns 3.13×.
- 2026-09-23 — D22 implemented in the shared helper (`vm/helpers.rs::IsoDate`): strict ISO
  fast path for `YYYY-MM-DD`, `YYYY-MM-DD HH:MM:SS`, `YYYY-MM-DDTHH:MM:SSZ`, everything else
  falls through to the chrono chain (which also switched from eager `.or` to `.or_else`).
  200k randomized strings agree with chrono. `date()` 180-240 ns → 48-56 ns on both VMs.
  D21 implemented: `LaneProgram::compile_typed(source, kind, &VariableType)` runs
  `TypesProvider` over the same AST and uses per-node types for load, field and closure
  element kinds (added to the differential test: typed from the first input, evaluated
  over all inputs). Fuzzing found one real bug: the constant pool deduplicated
  `Decimal`s by value, so `7.4` and `7.40` shared a slot and results lost scale; constants
  now compare by exact representation.
- 2026-09-23 — Stage 2b cell sets (`lane/cells.rs`, `CellSet`): unary cells over one value,
  empty cells always match, equality cells (incl. `'a', 'b'` lists) hashed by normalized
  value, comparison/interval cells on sorted boundaries with each region's rules computed
  by evaluating the real cell on a probe (exact by construction), other cells as lane
  programs over distinct cells; output is a rule bitset per row. Differential vs per-cell
  `run_unary(...).unwrap_or(false)`: 426k rule×value checks, 0 differences. Bench vs
  evaluating every cell per row: 100 rules 44×, 1,000 rules 436×, 8,000 rules 2,450×
  (cell set ~100-145 ns/row regardless of size).

- 2026-09-23 — Multi-expression programs (D27, D30): `LaneProgram::compile_many(&[(key,
  source)], chain)` compiles a whole expression node into one program. Loads shared across
  expressions are computed once (top level only, pinned); with `chain` each result is
  visible to later expressions as `$.key`. When no key contains a dot, `$.key` reads resolve
  statically to the producing register (no `$` object is built); otherwise `DollarBegin` /
  `DollarInsert` build it at runtime. `Stage` ops attribute a lane error to the expression
  that raised it; `evaluate_many` takes an optional row mask. `specialize` works on these
  programs too. Differential vs expression-node semantics (per-expression `Isolate`,
  chained `$`): 340,900 rows, 0 differences. Bench (97 generated expression nodes, 512
  rows): stack VM 219 ns/row median, lane width 1 147, width 64 90 — **1.46× / 2.32×**
  geomean (before static `$` resolution 0.80× / 1.18×).
- 2026-09-23 — Scale fuzz (200k cases per generator, ~150M comparisons plus 13.6M
  multi-expression rows and 1.13B cell-set checks) found one register-allocation bug:
  closure imports are pinned lazily on first reference and could reuse a register that
  was freed earlier in the same body, so an earlier write clobbered the captured outer
  value (`map([1,2,3] as y, map(list, o[#.x] in [y, #]))`). Pinned registers are now
  always fresh. Pinned as a special case; the rerun is clean.
- 2026-09-23 — Width-1 tuning round. Object literals keep static keys in the op (no
  per-key `string()` call): `{'a': 1, 'b': 2, 'c': 3}` 203 → 115 ns (stack 165). `x ??
  literal` is one `Coalesce` op (array/object fallbacks are deep-cloned per lane so a
  caller can never mutate the constant pool). `x in [consts]` / `not in` read the pool
  directly (`InConst`), no per-lane array clone. `some`/`all`/`none` start with a round of
  1 when a single list is live (width 1), so early exits cost what the stack VM costs;
  round bookkeeping reuses scratch vectors. `VariableMap` compares short keys (≤16 bytes)
  inline instead of calling `memcmp` through the dyld stub: this speeds up both VMs
  (`zz ?? 5` stack 37 → 32 ns). Corpus bench: width 1 **1.11×** geomean, width 64
  **2.14×**, columns **3.45×** (median 11.4 ns/row), expression nodes 1.53× / 2.37×.
- 2026-09-23 — Engine expression nodes on one program (feature `lane`): the handler
  compiles the node with `compile_many(chain)` once (thread-local cache keyed by the
  `Arc<Vec<Expression>>` identity, holding the Arc) and evaluates it in one call; on any
  lane error it re-runs the per-expression path so error context and partial traces stay
  exact, and in debug builds that path asserts it also failed (a lane-only failure is a
  bug, not a fallback). Bug found by profiling the engine bench, not by the fuzzer: in
  static `$` mode the fused `LoadCmp`/`LoadEq` paths (`$.x < 70`, `$.x == 'a'`) bypassed
  `$` resolution and read `$` from the scope, where static mode never builds it. Fixed
  (`load_of` refuses `$`-rooted paths in static mode); the multi-expression generator now
  emits `$.k` in every fused shape (comparisons both sides, equality, `in`/`not in`,
  ternary, logic, nested fields) and reproduces the bug without the fix (8,586 of 206k
  rows). Engine bench before the fix: 1.165× geomean (the affected nodes were silently
  running twice); remeasure pending. TODO: move the program cache from the thread-local
  onto compiled `GraphContent` (it grows with every distinct node seen by a thread).
- 2026-09-23 — Columnar round (profile first: of the columnar time, ~27% was the per-row
  `Result<Variable, IsolateError>` handoff, ~35% per-lane `set`/`take` kind dispatch, ~8%
  `rust_decimal`; a constant `-0.5` cost 12.9 ns/row).
  1. Typed output (`lane/output.rs`): `LaneRunner::evaluate_columns_into(program,
     columns, &mut Output)`; each 64-row chunk copies the output register's bank (Decimal
     slice, bit word, string bytes into offsets+data) and records boxed rows (sparse
     `Variable`s) and failed rows (sparse errors) as bitsets. `Output::get(row) -> Cell`,
     `variable(row)`, raw `numbers()/bools()/offsets()/data()/boxed()/failed()` for Arrow
     export. The `Variable` sink API stays.
  2. Bank kernels: `Const`, `SelectConst`, column loads (Dec/I64/Bool/Utf8/Strs),
     `Move`, `Merge`, `Coalesce`, `NullBranch` write the typed banks directly with no
     per-lane kind match. Decimal comparison has a same-scale fast path (`ops::order`:
     compare mantissas when scales match, else `Decimal::cmp`; exact, randomized test vs
     `Decimal::cmp`), used by both VMs, and `LoadCmp` on numeric columns compares without
     building a `Variable`.
  3. `Kind::Str`: lanes live in the generic slots as `Variable::String` and the kind
     guarantees unboxed lanes are strings (a separate `Symbol` bank would pay wrap/unwrap
     on every generic access). Sources: string constants, templates, string-returning
     calls, string/enum types from `TypesProvider`, speculation votes, Utf8/Strs
     columns. `string()` on a Str register is a copy. Output goes to the string buffer.
  Corpus (185 expressions, 512 rows): columnar with typed output **5.85×** geomean vs
  the stack VM (median 4.56×, 8.8 ns/row; was 3.44× with the `Variable` sink); columnar
  with the `Variable` sink 4.12×. Fused comparisons ~0.5 ns/row (~70×), numeric
  constants 0.9 ns/row, `x > 5` on a Decimal column 6.4 → 3.2 ns/row. What remains is
  allocation-bound (templates, arrays, `some` over array columns, string constants at
  ~10 ns/row because every row clones a `Variable::String` and copies bytes). Next
  levers: uniform (splat) registers so constants and loop invariants are not
  materialized per lane; dictionary-encoded string input/output; list columns for
  closures.
  Fuzz (new seed 31337, ~190M comparisons) found a pre-existing bug: `some`/`all`/`none`
  whose body assigns ran every element (sequential mode went through the non
  short-circuit path), so assignments from elements after the deciding one leaked
  (`v = a; w = all(['x', 'y'], # == 'y' and v = #; true); v`). Sequential short-circuit
  closures now go through the round path with one element per round. Pinned as special
  cases; rerun clean.
- 2026-09-23 — Category micro-suite (`benches/lane.rs` `LANE_EXPR`, now with `u:`/`us:`
  prefixes for unary cells with `$` = 5 / 'gold'; 73 expressions over closures, dates,
  strings, numbers/logic, unary cells). Fixes, most shared by both VMs:
  - Regex: `matches`/`extract` compiled the pattern on every call. Thread-local
    pattern cache (256 entries, errors not cached), used by reference because cloning a
    `regex::Regex` makes a fresh cache pool: `matches` 24 µs → 90 ns, `extract` 7.1 µs →
    170 ns, on both VMs.
  - `d()`: the ISO fast path now also runs in `parse_date` (strict ISO forms mapped with
    the same `from_local_datetime(..).earliest()`; everything else through the old
    chain, now `parse_text`). Randomized test over 4 zones incl. DST gaps. `d('2025-03-15')`
    327 → 80 ns, `isBefore`/`diff` ~410 → ~175 ns.
  - `format()`: strftime patterns parsed once (thread-local `Formats`); invalid patterns
    and write errors fall back to the original call so behaviour is byte-identical
    (test vs chrono). Small win (~230 → ~190 ns); chrono formatting dominates.
  - Intervals with literal bounds (`x in [1..10]`, unary `[1..10]`) are one `InRange`
    op (same `VmInterval::includes`, no allocation; non-number operands fall back to
    `membership` against the interval built once per chunk): 0.76× → **1.96×** width 1,
    3.4× width 64.
  - `x == k1 or x == k2 ...` over one load (the unary list `'a', 'b'`) is one `EqAny`
    op with `ops::equal` per constant (not `in`, whose errors differ): unary `1, 5, 9`
    1.02× → **1.78×**, five strings 3.27× width 1 / 5.2× width 64.
  - Calls/methods/templates take literal arguments straight from the constant pool
    (`Arg::Const`), templates concatenate into one reused buffer: `contains(x, '@')`
    0.73× → 0.92×, templates 1.34× → **2.23×**.
  - Closures: per-element results are plain `Variable`s plus a sparse ordered error list
    (was `Vec<Result<Variable, VMError>>`, a large move per element); a single live
    list in `some/all/none` enters the child frame once and loops elements with a mask
    reset only; multi-list rounds grow ×8. `map(items, # * 2)` 285 → 210 ns (1.46×),
    `all` 0.89× → 1.15×.
  Category geomeans (lane ÷ stack, width 1 / width 64): closures 1.22× / 1.55×, dates
  0.95× / 1.26× (the time is in the shared date code), strings 1.05× / 1.48×,
  numbers/logic 1.26× / 2.32×, unary cells now ~1.5× / ~3×. Corpus: width 1 1.16×,
  width 64 2.35×, columns (typed output) 5.93×; expression nodes 1.54× / 2.44×. Fuzz
  seed 2718 clean (~190M comparisons + 1.1B cell checks). Not done: `startOf`/`endOf`
  chain five chrono-tz local→UTC lookups (43% of the call); collapsing them changes DST
  edge results (`single()` per step), an exact UTC-only fast path is possible. Width-1
  closures that decide on the first element still pay ~30 ns of frame setup over the
  stack VM.
- 2026-09-23 — Leftovers and typed-numeric experiments.
  - `startOf`/`endOf`: the unit chains are generic over `DateTime<Tz>` and
    `NaiveDateTime` (`Calendar` + `Clock` trait); zones that are always UTC (UTC, Etc/UTC,
    GMT, Zulu, UCT, Universal) run the chain on the naive value and convert once, which
    is exact because UTC has no gaps or folds (randomized test: 50k instants × 3 zones ×
    8 units vs the chained steps). UTC `startOf('month')` 242 → 128 ns on both VMs; other
    zones unchanged by design.
  - Short-circuit closures: a single active lane skips scratch/list bookkeeping, rounds
    start at 1 element for every list (then ×8). First-element decisions width 1
    0.62-0.67× → 0.87-0.95×, width 64 0.75× → 1.23-1.46×; full scans unchanged or better.
  - Fused numeric kernels (item 2) built and REVERTED: evaluating the fused tree per lane
    (postfix code over a small `Decimal` stack, exact fallback region for boxed leaves,
    overflow and null-producing division) was slower than the per-op lane loops
    (`a * 1.5 + b` columns 8.6 → 13.8 ns/row, width 64 21 → 28 ns). Per-op overhead is
    ~0.5 ns/row; `rust_decimal` add/mul (~2 ns) is the floor. First attempt also showed
    leaves must be plain loads, otherwise error order changes (fuzz found it).
  - Integer fast path inside `Decimal` ops (item 1) measured in isolation: bit-exact
    (0 mismatches) but slower than `rust_decimal` (add 0.6-0.7×, mul 0.7-0.9×): mantissa
    extraction and `from_i128_with_scale` cost more than the same-scale add itself. The
    remaining numeric lever is staying in scaled `i64` across a whole column (item 3),
    which needs a new bank kind (the old branch's "no I64 tier" decision would need
    revisiting).
  - Load cost in row mode: in a six-load predicate at width 1, `get_hinted` + `path_with`
    are ~29% (~4 ns per key lookup); a shape-pointer check would bring that to ~1 ns, so
    shapes (item 4, D8) are worth ~15% on load-heavy row-mode expressions, paid for by a
    transition lookup on every object insert (including JSON → `Variable`).
  - Found while profiling: at width 1 the lane VM costs ~5 ns per op vs ~2.4 ns per
    stack-VM opcode, and `and`/`or` compile to Branch + Merge per operator:
    `true and true and true and true and true` is 0.48× the stack VM, real predicates
    (`type == 'event' and location == 'outdoor'`) ~1.0×. A fused predicate chain (one op
    evaluating `LoadEq`/`LoadCmp`-style terms left to right with per-lane short-circuit,
    so errors and evaluation order stay exact) would remove 2 ops per operator.
- 2026-09-23 — Width-1 dispatch, predicate chains, shapes measured, scaled-integer kernel.
  - Dispatch: `Executor::step` held every op and reserved 3,120 bytes of stack plus 12
    saved registers per call. Hot small ops (constants, branches, merges, moves, `Not`,
    `Stage`, `Num`/`Cmp`, `EqConst`, `SelectConst`, `NullBranch`) stay in `step`; loads,
    fused compares and `Coalesce` get their own `#[inline(never)]` functions; everything
    else is in `cold`. `LoadEq`/`EqConst` read their constant from the frame pool instead
    of building a `Variable` per call.
  - Predicate chains: `and`/`or` chains whose operands are all fused load comparisons
    (`x == k`, `x != k`, `x <op> number`, `number <op> x`) are one `Chain` op that runs
    each term only over still-undecided lanes (same short-circuit and error behaviour as
    the Branch/Merge form; `needs_rows` looks into the terms). `s == 'gold' and
    email == 'x'` width 1 ~1.0× → 1.5-1.66×, `s != 'x' and a >= 5` 1.51×.
  - Shapes (D8) measured before building, on 15-key objects with zen-types'
    `VariableMap`: hashed transitions cost +313 ns per object build (build itself 255 ns),
    a lean one-slot transition walk +68 ns (+27%); shape-checked lookups save 46 ns per
    15 reads (53.5 → 7.4 ns). Break-even is 1.5 reads per field per object with the lean
    tree, 6.8 with hashed transitions. Engine inputs are rebuilt from JSON per request and
    read ~1-2 times per field, so shapes on `VariableMap` now would be neutral for the
    engine and a cost for every other zen-types user. DEFERRED to the engine stage, where
    outputs are built from known shapes (D26) and the shape comes for free.
  - `Float64` column conversion formats into a 512-byte stack buffer instead of
    `format!` (bit-identical, randomized test incl. subnormals and extremes): 129 →
    57.5 ns per value, the dominant cost for pandas/Arrow float data.
  - Scaled-integer kernel (item 3, exact): a `+ - *` tree with an optional root
    comparison, ≥2 ops, leaves plain loads or literals, becomes one `Fixed` op that runs
    op-at-a-time over 64 lanes on `(i64 mantissa, scale)` pairs. Leaves come straight
    from `Int64`/`Decimal`/`Float64` columns or through the slot-cached row walk; a lane
    that is not a number, overflows, has a negative zero, or hits the both-operands-zero
    add/sub case goes to a fallback region (the normal compiled ops under a mask, result
    moved into the same register), so values, errors and scales are exactly
    rust_decimal's. Rules verified on 3M random pairs, 0 mismatches: add/sub rescale to
    the larger scale, mul adds scales (≤28), one zero operand returns the other operand
    (negated for `0 - b`), `0 * x` is plain `0`. Division and `%` stay out (rust_decimal
    normalizes those). The differential harness now also builds `Int64` columns for
    integer data. Width 64: `a * 1.07 * 1.2 + b * 0.5 - 3` 42 → 23 ns, `a * b - a * 4 +
    7 >= b` 55 → 30 ns; columns: 27.4 → 11.1 ns/row and 12.1 → 8.9 ns/row. Not yet
    vectorized: a uniform-scale path per op (all lanes one scale) would let the loop run
    over plain `[i64; 64]` slices.
  - Corpus: width 1 1.11×, width 64 2.31×, columns with typed output 5.67× geomean
    (sum 5,053 → 4,509 ns); expression nodes 1.45× / 2.41×. Fuzz seed 9001 clean.
- 2026-09-23 — Expression-side levers round (engine integration deliberately excluded).
  - Scaled kernel, uniform-scale path: each stack slot records whether all live lanes
    share one scale; add/sub/mul/compare on uniform slots run as straight loops over
    the frame width (rescale only the mismatched side, overflow collected as a lane
    mask, mul zero-operand lanes get rust_decimal's scale-0 zero afterwards), `Int64`
    leaves over contiguous rows are a slice copy with word-level validity, temporaries
    live in frame scratch, and frames narrower than 4 lanes use the fallback ops
    directly. Same-scale zero add/sub verified to give `(0, s)` for every scale.
    Columns: `a * 1.5 + b > 10` 12.1 → 4.9 ns/row (16×), `o.y.z * 2 + a` 4.7 ns/row
    (21×), `a + b + 3` 4.0 ns/row; width 1 unchanged (1.26-1.34×).
  - Division by powers of ten: NOT reproducible cheaply. rust_decimal's quotient scale
    is neither the plain shift nor the normalized shift (`0.001407 / 10` =
    `0.00014070`), 2M-sample test; division stays on rust_decimal.
  - Constant registers: string/dynamic constants get a pinned register filled once per
    (program, width); later chunks and later `evaluate_one` calls skip the fill.
    Long string constant width 1 35.6 → 13.8 ns (2.6×), width 64 6.3×; short string
    constants in columns 10.6 → 6.2 ns/row (the rest is copying bytes into the output).
  - Input formats: `Values::Dict { keys, values }` (codes into a child column; `LoadEq`
    on dictionaries up to 256 entries precomputes a match bitset per chunk) and
    `Values::List { offsets, child }` (rows materialize arrays lazily, only for loaded
    lanes). The differential harness builds dictionary columns for odd-length string
    keys and list columns for uniform number/string arrays.
  - Column specialization now infers element kinds under arrays/lists (`items[]`,
    `list[].x`): list child kind, or a 256-row majority vote over untyped columns. Before
    this, columnar closures ran generic compares: `count(items, # > 3)` columns 214 →
    134 ns/row, now equal to row mode at width 64.
  - `ryu` for float conversion: REJECTED. Its shortest digits differ from Rust's
    `Display` on ties (`-1.4190045091861383e15`), so the stack-buffer `Display` path
    (57.5 ns) stays. (Adding the dependency briefly re-resolved `Cargo.lock`; restored.)
  - Width-1 dispatch: `num`, `cmp`, `chain` and all non-trivial arms left `step`
    (frame 752 → 336 bytes); trivial expressions at parity with the stack VM (`true`
    ~0.95×, `a ?? 0` ~1.0×, `a + b` 1.13×), predicates 1.5×.
  - Date register kind (D23): not built. Width-1 date time is in shared chrono code
    (parsing, time-zone lookups, methods), which a typed kind would not remove; its
    benefit needs a timestamp column format first.
  - Corpus: columns with typed output 6.26× geomean (median 6.7 ns/row, sum 4,254 ns),
    width 64 2.62×, width 1 1.13×; expression nodes 1.47× / 2.44×. Fuzz seed 5150 clean.
  - Still open on the expression side: closures fed straight from list child columns
    (needs lazy list loads), timestamp columns plus a date kind, a persistent
    single-lane runner for the remaining width-1 fixed cost.
- 2026-09-23 — Why typed barely helps at width 1 (measured, `benches/lane.rs` `LANE_TABLE`).
  Profile of typed `a + b` at width 1 (35.6 ns, one fused op): ~12-15 ns fixed per call
  (frame entry, result handoff, `Variable` drop), ~4.5 ns in two `VariableMap` key
  lookups, the rest operand handling around a <1 ns decimal add. The old VM pays the
  same per-call floor and the same map lookups; typing only removes the `Variable`
  wrapping (~1-2 ns per op). Dispatch itself is ~2 ns per op (~20% of width-1 time).
  - Load operands (wasm3/Lua-style: numeric/compare ops reading a load site directly)
    built and REVERTED: width 1 gained ~2 ns (`a + b` 37 → 35.6 ns) but the per-lane
    fetch replaced the slice fills and vectorized loops, so width 64 (`a + b` 18.6 →
    24.3 ns) and columns (6.6 → 16.5 ns/row) regressed.
  - Compare-and-branch fusion not built: same ~2 ns ceiling per fused op.
  - Scaled kernel below 4 lanes: its fallback root now writes the kernel's destination
    directly (no `Move`); `a * 1.5 + b` width 1 1.01× → 1.13×, `a * 1.5 + b > 10` 1.12×
    → 1.21×.
  Conclusion: row-at-a-time over `Variable` inputs is at this design's floor; the width-1
  levers left are the per-call floor, load cost (shapes or loading shared inputs once per
  request in the engine), and batching engine requests so typed code runs at width 64 or
  over columns, where it already shows 2-30×.
- 2026-09-23 — Function calls, strings, dates and closures in columns. Profiles showed
  the functions' own work is a minority of columnar call time (`contains`: 16%; the
  rest is per-row `Variable` creation/drop, set/take, a registry lookup per row).
  - Calls and methods resolve their definition once per op (`ops::resolve_function` +
    `call_resolved`, same error texts).
  - `LoadCall`: a call whose first argument is a plain load and the rest literals reads
    the load itself. Dictionary columns evaluate the function once per distinct code
    (memo per op, cleared at the start of every columnar evaluation; a time-dependent
    input such as `d('now')` in a dictionary is evaluated once per batch). Text columns
    run `len`/`contains`/`startsWith`/`endsWith`/`upper`/`lower`/`trim` on the column's
    `&str`; list columns run `len`/`sum`/`avg`/`min`/`max`/`contains` on the child slice.
    Kernels only handle rows that fully succeed (valid, right types, no overflow, not
    empty); everything else goes through the generic function, so values and error
    texts are identical. The string/number cores are shared (`Text`, `Numbers` in
    `functions/internal.rs`) and the registered functions call them too.
  - Closures whose list is a plain load fetch it themselves; list columns feed elements
    straight from the child column (map/filter/count/one/flatMap and the short-circuit
    rounds) instead of materializing a row array.
  Columns vs old VM (plain = `&str`/pre-built values; dict/list = Arrow-style):
  `startsWith` 5.5× / 7.4×, `contains` 3.9× / 7.1×, `lower` 2.3× / 4.7×, `matches`
  1.4× / 14.6×, `date` 1.7× / 7.3×, `d()` 1.3× / 6.5×, `sum(items)` 1.4× / 5.8×,
  `len(items)` 2.6× / 5.5×, `some(items, # == 1)` 1.5× / 1.8×; closures with bodies
  (`map`/`filter`/`count`) stay ~1.6-2.0× (per-element `Variable` work), object/array
  literals 1.4-2.0× (a map or array per row). Fuzz seed 8080 clean.
  - Next: typed closure bodies over list children (write child decimals straight into
    the body's numeric register, read Bool results as bits), struct output for object
    literals with static keys (one typed output column per field), dictionary-aware
    membership (`s in [...]`) and memoized method chains (`d(x).year()`).
- 2026-09-23 — Lane owns its function and operator semantics (decision with Stefan: the
  old VM and its `functions/` stay untouched as the fuzz reference until the stack VM is
  deleted; the lane gets one typed implementation of everything).
  - `lane/builtins/`: every builtin is a list of typed overloads plus a no-match rule.
    `Arg<'a>` is a borrowed argument view (number/bool from typed registers, `&str` straight
    from text columns, list-column rows, or a borrowed `Variable`); `FromArg` converts
    strictly (no coercion, `Option<T>` for optional trailing parameters); `Out` returns
    typed results that `Frame::write` stores straight into the numeric/bool banks. The
    no-match error reproduces the old `Arguments` texts (`Argument on {pos} is not a …`,
    `… position is not a valid …`) from declared parameter kinds, or a function's own
    text (`Cannot determine len of type …`). Families: `text` (len, contains, upper,
    lower, trim, startsWith, endsWith, matches, extract, fuzzyMatch, split, own regex
    cache), `math` (abs, sum, avg, min, max, rand, median, mode, floor, ceil, round,
    trunc), `arrays` (flatten, merge, mergeDeep, keys, values), `convert` (isNumeric,
    string, number, bool, type, d), `legacy` (the 14 deprecated date/time functions),
    `dates` (all 30 date methods; `this` check and argument order replay the old
    `dynamic()`/`str()`/`ostr()` sequence). Calls and methods resolve their builtin once
    per op; `LoadCall` feeds column/list/dictionary rows as `Arg`s (the hand-written
    column kernels from the previous round are gone, the overloads are the kernels).
  - `lane/ops.rs`: the lane's own copy of the operator/value semantics (`Ops` associated
    functions: arithmetic, compare, equality, membership, truthiness, intervals, member
    access, slicing, assignment, elements); the stack VM keeps `vm/ops.rs`. The lane no
    longer calls any old function body or `vm/ops.rs`; still shared on purpose: function
    identifiers and registry arity/return metadata (compile errors stay identical) and
    value primitives (`VmDate`, `VmInterval`, duration/date parsing helpers).
  - The shared-helper routing from the previous round was reverted: `functions/internal.rs`
    differs from master only by the regex pattern cache.
  - New differential tests: every internal function with 1-2 arguments from a pool of 31
    argument shapes (32,736 calls, 696k comparisons) and every deprecated function and date
    method over 6 date subjects and 34 argument shapes (53,560 calls, 677k comparisons):
    0 differences, values and error texts, in every mode incl. dictionary/list/Int64
    columns. Fuzz seed 9191 clean.
  - Performance vs before the port (lane ÷ old VM, geomean): strings w1 1.03× → 1.24×,
    w64 1.47× → 2.03×, columns 1.77× → 2.87×; unary cells w1 1.41× → 1.60×; dates slightly
    lower (w1 0.93× → 0.86×, values convert Arg ↔ Variable around `VmDate`), everything
    else within noise. The typed representations that pay are next: a date register kind
    (timestamp + zone, parse once per load or dictionary entry), typed closure bodies over
    list children, struct/list outputs for object/array literals in columns.
- 2026-09-23 — Port regression fixed, then date registers, typed closure bodies, struct/list
  outputs.
  - Regression after the port (plain columns 20-35% slower, dictionary `lower` 0.41×):
    `Out::Str(Symbol)`, the dictionary memo stores `Out`, `Apply` takes `impl FnOnce`,
    `Builtins::call_hinted` tries the last matching overload first (per-op hint), and
    `LoadCall` builds column arguments straight from the column with a prebuilt argument
    list. Same-run ratios back to parity or better.
  - `Kind::Date`: a frame bank of `VmDate` values (`DateTime<Tz>` is `Copy`), `Arg::Date`
    (also produced by `Arg::of` for a dynamic date), `Out::Date`, `FromArg for &VmDate`.
    `d()` and date methods declared to return a date allocate date registers; `Dates::this`
    reads the bank; `d('...')`/`other` parse `&str` without building a `Variable`
    (`VmDate::text`, `helper::parse_str` shared with `parse_date`). A `Variable::Dynamic`
    is built only when the value escapes (generic ops, row results). `Output` gets a
    `dates` column (`Option<DateTime<Tz>>`, `Cell::Date`). Columns: `d(d2)` 65.9 → 40.8
    ns/row, `d(d1).year()` 83.8 → 63.4, `d(d1).isBefore(d(d2))` 112 (dictionary 43, old VM
    171).
  - Typed closure bodies over list columns: when every lane's list comes from a list
    column, items are `(lane, child index)` slots and the element register is filled by
    the same typed column loader as top-level loads (`fill_with`), no per-element
    `Variable`. Predicate closures (count/one/filter/all/some/none) with a typed bool body
    record results as `truths`/`settled` bitsets; spans that are fully settled are decided
    by popcount. Decisive closures over list columns start with 64-wide steps (non
    sequential). List columns: `count(items, # > 3)` 141 → 69 ns/row, `some/all/none` 155 →
    70, `map(items, # * 2)` 165 → 112 (old VM 260-300).
  - Struct/list outputs: a root object literal with unique static keys or an array literal
    compiles to a `Layout` tree (`Value(reg)` / `Struct` / `List`) instead of the final
    `Object`/`Array` op; leaves are ordinary registers (copied only when shared). Row APIs
    assemble the `Variable` from the layout (same key order as `Op::Object`); columnar export
    writes a recursive `Output` (`Shape::Struct(fields)` with one typed column per field,
    `Shape::List(child)` with offsets and a typed child when all items share a kind), so an
    Arrow/PySpark consumer reads decimals/bits/utf8/dates without touching `Variable`.
    `{x: a * 2, y: s}` columns 27.6 ns/row (old VM 163), `{x, y, z, w}` 32 (old 242),
    `[a, b, a + b]` 23 (old 92). Still `Variable`: fields whose value is itself dynamic
    (e.g. `map(...)` results, objects built inside closures) — typed list registers for
    closure results are the next step.
  - Gates: all zen-expression tests, fuzz seeds 9191/424242/77 × 3000, builtin and date
    differentials (with 25 new date flow cases and 15 new layout cases), a typed-output
    assertion test; clippy clean for lane code; Cargo.lock unchanged.
  - Follow-up: decisive closures over list columns start at step 1 again and then jump to
    64 (early exits `some(items, # == 1)` 62 → 23 ns/row, full scans 70 → 75). Column API
    snapshot (lane cols ÷ old VM, geomean over the 53-expression table): plain columns
    4.21×, dictionary + list columns 5.51× (before the builtin port 3.59×). The bench's
    `env $X` loops under zsh never set `LANE_LISTS`; earlier "dict" numbers in this log
    were dictionary-only.
- 2026-09-23 — Old VM restored to master (decision with Stefan: the stack VM stays exactly as
  on master; every improvement lives in the lane). `vm/` and `functions/` are byte-identical
  to master again (`vm/ops.rs` deleted, regex cache, date parse/format caches, IsoDate fast
  path and calendar shortcuts reverted there). The lane owns copies: `lane/date/` (date type
  `Date`, duration parsing, `helpers` with `IsoDate`, format cache, UTC calendar shortcut,
  their unit tests) and `lane/interval.rs` (`Interval`/`IntervalData`). `VariableMap` keeps
  master's lookups; only the lane-only `get_hinted` (with its short-key compare) is added.
  Remaining diff vs master outside the lane: `lane` feature + bench entry, `pub mod lane`,
  `intellisense::scope` visible to the crate, the `Isolate` lane hook, `get_hinted`.
  Differentials and fuzz now compare against master's VM: all green. `LaneCompiler::
  compile_typed` became crate-private (it exposed `TypesProvider`).
- 2026-09-23 — Shapes brought over: `core/types` `variable/{shape.rs,map.rs,mod.rs,ser.rs}` and
  `rcvalue/ser.rs` are taken verbatim from `feat/decision-compiler` (hidden-class
  `VariableMap`, `ShapeHint`, `MergePlan`); this branch had started from master without
  them. The lane's per-site `u32` position hints became `ShapeHint`s
  (`get_hinted(&mut hint, key)`); because a shape hint trusts its cached index, frame hints
  are now reset whenever the frame switches program (a stale hint from another program read
  the wrong key: `a ?? b ?? 3` returned `i`). The old stack VM code is still master's; it
  runs on the shaped map like every other `VariableMap` user. Gates: zen-types and
  zen-expression tests, fuzz seeds 9191/424242/77/31337 × 3000. Row-mode lane ÷ old VM
  ratios unchanged (both sides get faster lookups).
- 2026-09-23 — Assumption test for the typed-block redesign (prototype in the session
  scratchpad `laneproto/`, ~450 lines, deliberately unoptimized: per-op Vec clones, no
  register views). Plain op list interpreted per block, no fused ops; `i64`/scaled-`i64`
  numbers with branch-free overflow detection, bitmap booleans, borrowed strings, list
  children processed as one contiguous slice. ns/row, 8192 rows, block 1024, vs current lane
  columns (same values):

  | expression | current lane cols | prototype | × |
  |---|---|---|---|
  | `a` | 2.2 | 0.28 | 8× |
  | `a + b` | 6.8 | 0.68 | 10× |
  | `a * 1.5 + b` | 5.3 | 1.41 | 3.8× |
  | `a * 1.07 * 1.2 + b * 0.5 - 3` | 8.4 | 2.78 | 3.0× |
  | `a > 3` | 4.3 | 0.67 | 6.4× |
  | `a > 3 and b < 10` | 7.9 | 1.53 | 5.2× |
  | `a * 1.5 + b > 10` | 5.0 | 1.88 | 2.7× |
  | `s` | 11.0 | 0.19 | 58× |
  | `s == 'gold'` (non-dictionary) | 5.9 | 2.85 | 2.1× |
  | `lower(s)` | 28.6 | 5.7 | 5.0× |
  | `count(items, # > 3)` | 70 | 5.5 | 13× |
  | `map(items, # * 2)` | 111 | 4.3 | 26× |

  Block size: 64 → 256 → 1024 gives ~1.5× on numeric programs, flat beyond 1024. Varied
  data (random values, mixed strings, lists of 4-12) behaves the same. Not yet covered by
  the prototype: Decimal fallback rows (only detection is costed), rust_decimal's zero/sign
  quirks (known rules from the `Fixed` kernel), division, errors, nulls beyond validity AND.
- 2026-09-23 — Typed-block redesign, stages 1–2 (numbers, strings).
  - Numbers: `Kind::Num` registers hold scaled integers (`mant: i64`, `scales: u8` per lane)
    with a per-register `wide` bitset for lanes that need a `Decimal` (overflow, >64-bit
    mantissa, negative zero). `lane/scaled.rs`: `Scaled` (per-value rules) and `Kernel`
    (slice kernels: add/sub/mul with branch-free overflow flags, compare via i128 when scales
    differ). rust_decimal's zero rules measured and reproduced exactly (a zero left operand
    yields ±b with b's scale, a zero right operand yields a, `0 * x` is plain 0). Numeric
    builtins abs/floor/ceil/round/trunc run on scaled ints (negative-zero results fall back).
    `Scaled::write` formats numbers without Decimal. Unit tests compare every kernel with
    rust_decimal on random mantissas/scales incl. zeros and overflow edges (1.3M+ checks).
  - Dense column loads (top-level frames, full blocks): I64 is a slice copy, Dec splits to
    parts per value, Bool reads the bitmap word. Output numbers are exported as
    mantissa/scale (`Output::mantissas`, `Output::scales`; wide rows boxed).
  - Deleted fused ops: `Fixed` (+`FxOp`, `Fx`, gather), `Chain` (+`Term`), `LoadCmp`, and
    numeric `LoadEq`; the plain load + typed kernel is as fast or faster.
    `InRange`/`EqAny`/`InConst`/`EqConst` numeric paths use the compare kernel.
  - Strings: `Kind::Str` registers are byte spans into a per-frame `arena: String` (reset
    past 1 MB, pinned constants refilled via the `filled` memo). Utf8 column blocks are
    validated once and copied with one memcpy; Str export copies contiguous spans at once.
    Typed text kernels (`TextKernel::{Test, Map, Size}`: contains/startsWith/endsWith,
    lower/upper/trim, len) run on spans; lower/upper on an ASCII block is one
    make_ascii_* pass; trim of already-trimmed text reuses spans. `Concat` op for Str + Str,
    `Join` writes into the arena, `string(number)` formats scaled ints directly,
    `SelectConst`/`Coalesce`/`EqConst`/`EqAny`/`InConst` have text paths.
  - `LoadCall` keeps the dictionary memo; for other columns it performs a typed load into a
    scratch register and runs the regular typed call op.
  - Bench tables are now Arrow-style Utf8 columns; the differential harness adds Utf8
    string columns (keys with length % 4 == 0) and a Unicode `text` column in the builtin
    differential.
- 2026-09-23 — Typed-block redesign, stage 3 (lists, closures) and block overhead.
  - `Kind::List` registers: lanes hold item ranges into a frame-level `items: Vec<Item>`
    (`Item::{Num(m, s), Text(span), Bool, Value}`), reset together with the text arena.
    Map/filter closures allocate list registers; count → `Kind::Num`, one/some/all/none →
    `Kind::Bool`. Values materialize to `Variable::Array` only on escape; builtin call sites
    box list lanes first (`Frame::box_lists`), arrays written by fallback paths stay boxed.
  - Closure sweep over list columns (`Executor::sweep`): when every active row has a list and
    the child ranges are contiguous (and the body is pure), element registers are filled
    straight from the child column slice, predicate results are kept as bits
    (count/one/some/all/none decided by popcount per row, typed writes), map results are
    written as items, filter copies child values as items. Anything unsettled (errors, non-
    bool predicate results) re-runs on the general path.
  - List reductions: sum/avg/min/max over list columns (I64/Dec children) and list registers
    fold scaled ints with the exact rules (avg divides in Decimal like the builtin);
    `LoadCall` keeps list columns on the list-view path.
  - Output: `Kind::List` exports as `Shape::List` with an append-only typed child
    (`Output::push_item`); boxed/failed list rows close with empty ranges.
  - Bench: `LANE_ROWS` sets the row count; list columns of integers use an I64 child.
  - Open: blocks are still 64 rows. At 8192 rows `a + b` is 1.7 ns/row vs 0.68 in the
    1024-block prototype; about half of the gap is fixed per-op/per-block cost and half is
    block width. Widening frames means multi-word masks through every op (~100 lane loops).
  - Export per register per block for struct fields (`export_shape` → `export_value`);
    fixed-width array literals of numbers/bools export strided. `{'a': a, 'b': b}` 19.2 →
    1.4 ns/row, `[a, b, 3]` 31 → 4.6.
  - Snapshot (8192 rows, lane ÷ master's VM, geomean excl. `matches`): plain columns 7.8×,
    dictionary/list columns 9.4×, row objects ×64 2.4× (before the redesign: columns 4.5×).
    Loads 25-90×, comparisons/logic 19-22×, arithmetic 10×, strings 13×, membership 18×,
    closures over list columns 6-14×; slow spots left: division (Decimal), dates on plain
    strings (parse/tz), closures over row objects, `max([a, b])` literal arrays, absent
    columns with `??`, string functions on dictionary columns without memo reuse.
- 2026-09-23 — 1024-lane blocks for columns (stage 4).
  - `lane/mask.rs`: `LaneSet` trait with two implementations: `Mask` (up to 1024 lanes,
    16 words, operations touch only the words in use, single-word fast paths) and `Word`
    (one u64). `Frame<M: LaneSet = Mask>` and every executor function are generic over the
    lane set: row APIs (`evaluate_one/with/many`) run `Frame<Word>` in 64-row chunks, the
    columnar path runs `Frame<Mask>` in 1024-row blocks. A single wide mask type was tried
    first and made width-1 calls 2-4× slower (every mask op copied 136 bytes).
  - All per-lane bitsets (bits/boxed/wide/filled/masks/alive) are lane sets; lane loops use
    `Lanes::of`; kernels producing bits (`Kernel::compare`) fill words per 64-lane chunk;
    output bitmaps are written word by word (`LaneSet::store`); column validity/bool words
    come from `Column::valid_mask`/`Column::mask`; per-block stack arrays moved to frame
    buffers. Bitsets are reset whenever the frame width changes.
  - New differential test over 2500 varied rows (two full 1024 blocks and a partial one).
  - A/B against the 64-lane build (same inputs, best of two alternating runs, 8192 rows):
    columns 1.14× geomean overall; comparisons/membership/conditionals 1.5-2.7× (`a in
    [1, 5, 9]` 4.0 → 1.5 ns/row, `a > 3` 1.7 → 0.9, `flag ? a : b` 2.2 → 1.2); kernel-bound
    work (strings, dates, closures) neutral; per-lane Decimal/Variable paths (division,
    modulo, templates, `max([a, b])`, closures over row objects) 7-12% slower. Row APIs
    (Word frames) within noise, membership in typed single-row mode ~10% slower.
    Branch-free lane indexing (`lane >> 6 & 15`) and full-width `copy_typed` slice copies
    added after the first A/B.
  - Final snapshot (8192 rows, best of two, lane ÷ master's VM, geomean excl. `matches`):
    columns 10.0×, dictionary/list columns 12.0×, 64-row batches 2.4×, single row 1.0×
    (typed and untyped). Single-row arithmetic/conditionals are 0.7-0.8× master (per-op
    setup of the typed kernels at width 1); comparisons/strings/dates/membership at or above.
    Open: width-1 kernel setup, dictionary string columns (per-lane arena fill), division,
    date parse/tz, `map(objs, ...)` over row objects, `max([a, b])`, `zz ?? 'default'` on
    absent columns.
- 2026-09-23 — Narrow-frame arithmetic: frames under 8 lanes run add/sub/mul/compare per
  lane on the scaled ints (`lane_num`/`lane_cmp`) instead of the slice kernels, removing the
  kernel setup that made typed single-row slower than untyped. Typed single-row `a + b`
  61 → 47 ns, `a * 1.5 + b` 80 → 57, `a > 3 and b < 10` 90 → 68 (untyped: 53/82/129,
  master: 35/45/57). The rest of the single-row gap to master is the lane's per-call cost
  (frame entry, field lookups, result), not typing. Mixed-scale add and multiply kernels
  were rewritten branch-light with 64-bit overflow checks; column timings unchanged (the
  loops are bound by 64-bit multiplies, which NEON cannot vectorize), and per-op setup is
  ~15% of an arithmetic chain, so direct-to-destination writes were not pursued.
- 2026-09-23 — Column follow-ups (ns/row at 8192 rows; the machine was loaded, same-run
  numbers compared):
  - List columns: `len(items)` from offsets 19 → 3.7; sum/min/max over Int64 children as
    slice folds (sum 20 → 6.1, max/min 16.6 → 15.4 with a Dyn result register); avg divides
    the integer sum in Decimal like the builtin.
  - `??` with a constant takes the constant's kind when the left side is untyped; null
    lanes are filled in one step; absent columns load as a bulk Null fill (`zz ?? 'default'`
    16 → 4.5). String export of a block whose rows share one span writes the text by
    doubling copies (`'approved'` 3.3 → 0.8).
  - `max([...])`/`min([...])` over a literal array of numeric operands compile to
    `Op::Extreme` (scaled compare, last-max/first-min like the builtin, builtin fallback
    for non-numeric lanes): 80 → 5.2 (single row 129 → 42).
  - Dictionary strings: loads keep a per-site code → arena span cache (per evaluation,
    reset with the arena); string-returning memoized calls reuse the stored span per code;
    cheap text tests/sizes run on the cached load instead of the memo. `s` 11.7 → 2.4,
    `lower(s)` 10.9 → 4.5, `len(s)` 7.2 → 3.0, `s + '-' + s` 32.6 → 13.7,
    `s in [...]` 15.2 → 5.4.
  - Closures over list columns with pure bodies (no imports, no env loads) skip the
    per-element parent/row vectors: count/all 18 → 10.5, map 55 → 48, filter 41 → 33.
  - Dates: `d(text)` reuses the previous lane's parse when the text is byte-identical;
    number parts (year/month/day/...) and plain comparisons (isBefore/isAfter/isSame...)
    on date registers skip builtin dispatch and write typed results. Distinct values per row
    (`LANE_VARY=1`): `d(d2)` 49 → 30, `.year()` 69 → 36, `.isBefore()` 119 → 63,
    `.startOf()` 73 → 44. A per-day timezone-offset cache was rejected (not provably exact).
  - Division/modulo: exact scaled paths verified against rust_decimal — division only when
    the dividend scale ≥ divisor scale and it divides exactly (rust_decimal keeps extra
    trailing zeros otherwise), remainder only when non-zero (with |x| < |y| returning x).
    Others stay Decimal.
  - Templates of text parts build all lanes into one buffer: 25 → 18.9.
  - Bench: `LANE_VARY=1` gives each row distinct a/b/s/d1/d2 values.
  - Not done: list-of-struct columns (`map(objs, #.x)`) — needs a struct column type and
    element-field loads bound to the child columns inside closure bodies.
- 2026-09-24 — Date arithmetic and typed list items (ns/row at 8192 rows, `LANE_VARY=1`
  for dates, `LANE_LISTS=1` for lists):
  - Dates: `add`/`sub`/`startOf`/`endOf` with constant arguments are resolved once per op
    (`Dates::shift` → `Shift`) and applied to date registers directly. Zones that are
    always UTC calendar-shift on `NaiveDateTime` (exact: no gaps/folds; randomized test vs
    the chained tz steps). `d(d1, 'UTC').add(1, 'd')` 186 → 29.6 (master 186),
    `.startOf('month')` 256 → 25.2, `.add(2, 'month').endOf('day')` 321 → 43.3. Local and
    named zones stay on chrono-tz (`d(d1).add(1, 'd')` ~100, 2.4× master).
  - List registers store items as a struct of arrays (`Items`: tag, payload word, scale,
    side `Variable`s) instead of a `Vec<Item>` enum. `map` with a clean Num body appends a
    block with slice copies; `filter` over Int64 children compacts branch-free; export of
    a block whose list spans are contiguous and all-numeric is one bulk child copy plus
    offsets; `sum`/`min`/`max` over uniform-scale numeric items fold the mantissas (a zero
    sum at scale > 0 falls back to the exact path). `map(items, # * 2)` 48 → 19.7,
    `filter(items, # > 3)` 33 → 24.4, `sum(map(items, # * 2))` 32.8 → 25.9,
    `sum(filter(...))` 41.6 → 29.9, `map(items, # + 0.5)` 29.8 → 23.4. Master 300–470.
- 2026-09-24 — `max`/`min` over a list register get a Num destination (35.7 → 24.9 ns/row
  for `max(map(items, # * 2))`). Long differential campaign vs the old VM:
  - New `lane_fuzz_columnar_matches_stack_vm`: mixed-scale decimals, values near i64
    limits, `-0`, zero-sum decimal lists, Int64/Decimal/string list columns, dictionary/
    Utf8/plain strings with unicode, dates in 7 zones around DST edges with
    add/sub/startOf/endOf chains, 1–2500 rows (block edges 63/64/65/1023/1024/1025).
    Knobs: `LANE_FUZZ_SEED`, `LANE_FUZZ_ITERS`, `LANE_FUZZ_ROWS`, `LANE_FUZZ_CASE` +
    `LANE_FUZZ_EXPR` (re-run one case's rows with another expression), `LANE_FUZZ_SHOW`,
    `LANE_TZ` (process local zone for the whole suite).
  - Harness fixes: integer list columns now build Int64 children (the Int64 filter/map
    paths were not reached before); column rows go to the stack VM as the `Variable`
    (the JSON round trip turned a decimal `-0` into `0`).
  - Bug found and fixed: closure bodies at one depth share a frame, and memo/dictionary
    caches were indexed by per-program slots, so sibling closures reading dictionary
    columns reused each other's cached results (`[none(ints, contains(s, '')),
    count(decs, len(s) > 1)]`). Caches are now parked per program id in the frame.
  - Result: 164 runs, ~934M comparisons, 0 differences — columnar 40 seeds × 2000 (UTC)
    + 36 × 1500 (New_York/Berlin/Lord_Howe), generic 40 × 5000, typed 40 × 5000,
    many 5000, cell sets 3000, full suite under 5 zones. Script:
    scratchpad `campaign/run.sh` (not in repo).
- 2026-09-24 — Conformance suite (`core/expression/tests/spec/`, README inside): 8335
  cases across 9 chapters (literals, arithmetic, comparison/logic, access/assignment/
  scoping, closures/collections, math/aggregates, strings/conversions, dates/DST, unary).
  Expected outputs are the intended semantics with exact-decimal comparison; deviations of
  the stack VM are recorded in place as `bug:` (318 rows) or `question:` (341 rows).
  Runners: `tests/spec.rs` (stack VM vs spec, bug rows must keep failing until fixed),
  `lane_matches_spec` in `tests/lane.rs` (every lane path vs the stack VM's actual result,
  panics caught). Lane bug found and fixed: bracket keys containing `.` (`a['a.b']`) bound
  to the dotted column `a.a.b`; load paths with a dotted segment no longer bind to columns.
  Lane matches the stack VM on every case under UTC, Europe/Berlin, America/New_York and
  Australia/Lord_Howe.
- 2026-09-24 — Conformance suite round 2: 28112 cases in 110 files (2.9 MB). New
  chapters 10 (builtin × type × arity), 11 (null/error propagation), 12 (business rules),
  13 (nested data, lists past 64/128), 14 (numeric properties), 15 (string pipelines,
  regex), 16 (date matrices by unit × operation × anchor × zone), 17 (programs), unary
  10–17 (decision-table cells). Several chapters were generated by independent Python
  reference models (scratchpad, not in the repo). 599 `bug:` / 621 `question:` rows. New
  stack-VM bugs: `rand(x)` panics for negative x, `a % b` drops the divisor scale when
  |a| < |b|, and objects past 32 keys (VariableMap spill to ahash) lose insertion order
  nondeterministically (same on master). `lane_matches_spec` now evaluates the stack
  twice and skips cases whose stack result is nondeterministic (91 cases). Lane matches
  the stack on every other case under 4 zones.
- 2026-09-24 — Object results built from a per-program `Shape` (`VariableMap::from_shape`,
  distinct keys ≤ 32, same reversed key order as the stack VM): `{'a': a, 'b': b}` single
  row 90.6 → 82.1 ns. Dead code removed (`Frame::result`, `assemble`, `set_num`, the
  unreachable `Const` fallback in `step`) and `exec.rs` (4456 lines) split into
  `lane/exec/`: mod.rs (run/enter/execute/step/cold), frame.rs, export.rs, context.rs,
  load.rs, number.rs, text.rs, date.rs, closure.rs. Moved verbatim (line-multiset check),
  only `pub(super)` visibility and per-file imports added; largest file is closure.rs
  (~940). Remaining dedupe candidates (≈450 lines: one row-load visitor, frame write
  helpers, closure bit/range helpers, cold-arm helpers) are listed in the review and not
  done yet.
- 2026-09-24 — exec dedupe after the split, each item checked against the code first and
  A/B-benchmarked (interleaved runs vs a pre-dedupe copy, medians of 5) before keeping it.
  Kept (parity on row, batch and column paths): `put_scaled`, `put_mask`, `intern`,
  `fill_typed` (coalesce), `Frame::split` (Branch), `NumOp::scaled` / `NumCmp::test`
  (`#[inline(always)]` — plain `#[inline]` was not honoured on the width-1 path),
  `Mask::bit`, `Column::range` + slimmer `list_children`, `ClosureFunction::holds` and
  merged `finish` arms, one `lookup` for `row_value`/`load_eq`, `simple(path)`,
  `path_with(FnOnce)` replacing `path_eq` and `walk` (hints taken out of the frame for the
  op), `Frame::block` for contiguous spans. Reverted because they cost speed: `set`
  delegating to `store` (+10% on closures), `copy_typed` split per array (3 lane loops),
  iterator-based `ones`/`tally`, helper-based `cold` arms and `env`/`with_row_value`/
  `load_call` rewrites (changed inlining, +2–12 ns single row). Frame caches kept as
  separate fields. Result 4606 → 4560 lines, post/pre geomean 1.00 on all paths.
  Found while measuring: `d(d2)` columns is bimodal 8.4 / ~19–20 ns depending on process
  history (both builds; e.g. after 15 unrelated programs) — layout or cache-state effect
  in the column date path, not yet investigated.
- 2026-09-24 — asm/profile-guided round (each change: direct lane-vs-stack check on exact
  Decimal/I64/nullable columns + interleaved A/B, medians of 5; ns/row at 8192 rows):
  - Kept: `nums` copied only for wide lanes; `copy_typed` Num partial path copies whole
    64-lane words with one slice move when the mask word is full (`a ?? 0` 1.9 → 0.8).
    Constant comparisons use `Threshold` (constant rescaled once, or floor/ceil when the
    constant has more decimals; non-representable `==` is all-false) +
    `Kernel::threshold` (`a > 3.25` 1.5 → 0.8). Negative literals (`-2.5`, `+x`,
    parenthesized) fold to constants via `Const::number` (`a <= -2.5` 11.1 → 0.9,
    `a * -1` 11.2 → 1.7, `a + -3.5` 11.6 → 2.1); intervals with negative/fractional-literal
    bounds use `InRange` (`a in [-10..0]` 50.8 → 1.9); typed `Negate` (`-a` 10.4 → 2.6,
    zero and i64::MIN go through Decimal so `-0` matches the stack VM); mask packing 8
    compares at a time via the 0x0102040810204080 multiply (~10% on comparisons).
  - Tried and reverted: concat via `String::extend_from_within` (+70%) or `push_str`
    (+48%) — per-piece memcpy calls lose to the byte loop for short strings; branch-free
    `uniform` (no gain). UTF-8 is already validated once per block (`texts()`).
  - Not done: exact chrono-tz offset cache (transition bounds are internal); `map` result
    write-through to Output and zero-copy column operands (design changes).
- 2026-09-25 — Bug hunt (6 agents, `lanecheck` tool in the scratchpad: old VM vs every lane
  path from JSON rows, exact numbers, `--columns`, `--strings`, `--tz`, unary). 14 confirmed
  issues, all fixed except interval interop; regression tests `lane_regression_*` in
  tests/lane.rs:
  - Object shapes were cached by `fields.as_ptr()` — clones of a program could pick another
    struct's keys. Now numbered in a fixed pre-order walk (`collect_shapes` /
    `assemble_taken(.., Some(&mut 0))`); nested `cell` assembly uses plain inserts.
  - `compile_many`: assignments leaked between entries. New `Op::Rewind` before each entry
    (dynamic + assigns) resets the row env to its root scope and re-adds `$`.
  - `Columns::row` inserted dotted columns into shallow clones of `Values::Any` objects
    (mutated caller input) — Any values are deep-copied now.
  - `Items::truncate` left `values` behind after a bailed sweep (unbounded leak); parked
    caches grew one entry per new program on the row APIs (capped at 8).
  - `Column::number` had no `Values::Any` arm (sum/avg/median/mode over List<Any> failed);
    dictionary memo cached `rand`; `find` took the first duplicate column while `row()`
    took the last; `CellSet::probe` overflowed near Decimal::MAX; `compile_many(&[])` had
    no out register; >20k-node expressions now error cleanly instead of wrapping u16
    counters.
  - Lane dates are emitted as `VmDate` and `VmDate` inputs are accepted (`Date` is `Copy`,
    `as_date` returns owned). Intervals still differ (`VmInterval` is private to `vm`).
  - Malformed Arrow input: string blocks check monotonic offsets + char boundaries
    (boundary scan only for non-ASCII blocks, ~0.2 ns/row); list ranges clamp decreasing or
    negative offsets to empty (no wrap/hang).
- 2026-09-25 — Bug hunt round 2 (6 agents: strings, typed, unary/CellSet, nested, dates,
  grammar fuzzer ~330k expressions + 1.1M chain rows). 10 confirmed, all fixed; regression tests
  `lane_regression_chained_dollar_matches_isolate`, `_cells_survive_invalid_neighbours`,
  `_cells_probe_between_close_bounds`, `_malformed_offsets_on_fast_paths`:
  - Chained `compile_many`: `$.k0[1]` read the env `$` in resolvable mode (`member_fast` now
    refuses `$` once entries exist); stage 0 saw `$ = {}` (`DollarBegin` removed, first insert
    creates it, like `Isolate::insert_dollar`); `$` lost after a dotted assign or `$ = x`, and
    `$root.$` missed earlier entries — dynamic chains now keep owned root scopes
    (`Program::chain`, `Context::roots: Envs`) that `DollarInsert` writes and `Rewind` restores.
  - `CellSet`: one uncompilable Other cell dropped all Other cells (now skipped individually);
    region probe uses `a + (b - a) / 2` and must fall strictly between the bounds.
  - Non-monotonic Utf8/LargeUtf8 offsets: `Column::texts` rejects the window (LoadEq fallback);
    `fill_dense` no longer subtracts offsets once the window is known non-monotonic (debug panic).
  - List offsets past the offsets array: builtin args read `[]` like `Column::variable`;
    `filter` over List<I64> uses the bulk `extend_selected` only when the range is in bounds.
  Shared with the old VM, recorded in tests/spec instead of fixed: invalid strftime specifiers
  panic (08-format, 7 more `bug:` rows); a valid date past chrono's local range panics when
  rendered (10-05-dates, 3 `bug:` rows; the harness now renders results inside the panic guard);
  parse time grows ~4x per paren level around a closure call over an interval (01-syntax comment
  + rows).
- 2026-09-29 — Merged origin/master (8 commits). Behavioural change for expressions is #529
  "date input": dates act as their text in string contexts, compare with strings, parse more
  ISO shapes, keep offsets as instants, resolve DST gaps forward, `d(date, zone)` converts, and
  engine date inputs are `VmDate`s that keep their source text (`DateValue::from_text`).
  - Spec: 74 changed rows re-baselined (16 fixed `bug:` notes dropped, 58 new intended outputs,
    stale date questions removed); new chapters `standard/18-date-inputs.csv` (623) and
    `unary/18-date-inputs.csv` (75) via the harness literal `date('...')`; harness line numbers
    after `#` comments fixed. 28,828 cases, 598 `bug:` rows.
  - Lane port: master's parser copied into `lane/date` (`parse_text` = ISO fast path +
    `parse_slow`, `resolve_local`/`skip_gap`); `Date::{coerce, matches, textual, source}`;
    generic ops mirror `VmDate::textual`/`matches`/`coerce` (fetch, path steps, add, slice,
    `in` object keys, equality/ordering/membership with strings); sourced dates stay boxed
    (`Arg::Var`, never a typed Date register) so their text survives; builtins retry with dates
    as text when no overload matches (`Builtins::textual`, args from 1 then from 0);
    `string()`/`bool()`/`d(x, zone)`/`contains`/`fuzzyMatch`/date-method string args/legacy
    date functions follow master.
  - Perf (A/B vs the pre-merge tree, medians): UTC `Z` timestamps 7–20% faster; local-time
    parses +2–4 ns/row (columns +10–20% on parse-only expressions), `items[0] + a` rows ~+8%;
    non-date expressions neutral. Profiles of `d(d2)` show the same function mix in both
    builds, so the parse regression is not in one function — not pinned down further.
- 2026-09-29 — Spec-wide benchmark (scratchpad `specbench/`: every spec expression, 128 cycled rows,
  old VM vs lane 1-row / batch / specialized / columns, best of 2 runs). Before → after this round
  (geomean old/lane): 1 row 1.06→1.61×, batch 1.70→2.87×, typed 1.76→2.97×, columns 1.93→4.62×.
  Split: input-reading expressions 1.03/1.52/1.61/1.61× → 1.07/1.60/1.69/1.76×; constant
  expressions (56% of spec expressions) → 2.2/4.5/4.6/9.7×.
  - Compile-time constant folding (`LaneCompiler::fold`, memoised `pure`): maximal pure subtrees
    that evaluate to a scalar or a date fold to a `Const` (new `Const::Date`); errors are left to
    run time; `rand`, `d()`/`d('now')`/zone names, `isToday/isYesterday/isTomorrow`, deprecated
    date functions and closures never fold; `format` folds only when its pattern renders.
  - `Op::LoadIn`: `x in <path>` scans List<Strs/Utf8/I64/Dec/Bool> columns in place (was a
    per-row array rebuild, 12× slower than the old VM); everything else falls back to
    `Ops::membership`.
  - Row strings longer than 256 bytes stay boxed instead of being copied into the arena.
  - A 3× floor is not reachable: work both engines share (Decimal pow/format, array/object
    construction, merge/flatten, env cloning for assignments) and per-row object rebuilding for
    paths that cannot bind to a column (`a[0]` on a struct, `$root`, `values(o)`, closures over
    mixed-type lists) set the p10. Next lever: build only the referenced part of a row.
- 2026-09-30 — Cleanup for master: `core/engine` restored to master (decision-compiler
  leftovers `compiler/*`, `data/*`, `impact.rs`, their tests and the feature-gated expression
  node hook removed). Dead `zen-types` API removed with them (`MergePlan`/`MergeOp` patch
  planner, `insert_hinted`, `value_at(_mut)`, `Shape::fingerprint`, `contains_reference`);
  lane `Column::valid_word` removed. Old VM, engine and `Cargo.lock` identical to master; pure
  master passes the same 28,828-case spec with the same 590 known bugs.
- 2026-09-30 — Stage 3.0 + 3a (graphs, `Variable` boundaries) in `core/engine/src/compiled/`:
  - Built by `GraphContent::compile` (`compiled_plan: Option<Arc<CompiledPlan>>`, verdict via
    `Decision::compiled_verdict`); `Decision::evaluate_with_opts` uses it when not tracing,
    `Decision::evaluate_batch` is new; everything else stays on the walker.
  - Exact walker order by **replay**: `GraphWalker::next_with` takes a switch oracle; the
    compiler replays the real walker with scripted switch outcomes into a lazily grown schedule
    tree (`schedule.rs`: `Segment { events: Execute { node, parents, visible }, end:
    Finish(ending) | Switch { children } }`); rows are partitioned by switch outcome at run time.
  - Expression nodes = one `compile_many(chain)` program; tables = `CellSet` per field column,
    distinct standard programs per plain column, per-rule output programs, hit policies First /
    Collect / First+collect columns (`table.rs`), candidate bits iterated sparsely; switch
    conditions = lane programs; transform attributes (inputField, loop as flattened items,
    outputPath, passThrough) shared by expression and table nodes; input/output nodes call the
    real handlers (validation, dates); `$nodes` built only for nodes whose sources read `$nodes`
    or `$root`; `inputField` error text reproduced through `Isolate` on the error path.
  - Walker fallback verdicts: function (10 fixtures), decision (3), custom (1) nodes; tables
    whose output cells or field-less cells read `$`; unparsable expression-node sources.
  - Gates: `tests/compiled.rs` differential (97 of 113 fixtures compiled, 2,009 inputs, single
    and batch, identical to the walker incl. error text), engine suites 412/412.
  - Perf (256 rows, walker with its opcode cache + table indexes as baseline): single 1.39×,
    batch 2.82× geomean; 8k table 1.97× / 4.29×. Profile: remaining time is `Variable`
    merges/object building between nodes (memmove, shape transitions, clone/drop) — stage 3b.
- 2026-09-30 — Stage 3b (committed `ad557f63`): column-major node data (`compiled/data.rs`:
  `Shape::Plain | Patched{base, layers, leaves}`, leaves bound as lane columns through
  `evaluate_bound`), table first-hit output as slot leaves with presence masks, dotted `$`
  resolution in `compile_many`, deferred first-hit verification of non-indexable cells
  (`CellSet::evaluate_indexed` + `test`, memo per row × cell, rule-major on open rows only),
  single rows routed row-major. Perf: single 1.32×, batch 3.45× geomean (hazardous-materials
  0.85→1.85× batch).
- 2026-09-30 — Old IR re-measured on the same 91 fixtures / inputs / machine
  (`feat/decision-compiler`, scratch worktree, same harness):

  | engine | single | batch |
  |---|---|---|
  | old IR, Optimized tier (typed arenas + kernels) | 3.39× | 7.66× |
  | old IR, Cold tier (same IR, every expression a per-lane `Isolate` leaf) | 1.59× | 1.89× |
  | lane engine (3b) | 1.29× | 3.30× |

  Finding: the old IR's structure (slots, joins, schedule) alone gives 1.89×; its speed is
  typed values that stay in registers across nodes and are boxed once at assembly. Porting
  the IR with lane leaves ≈ Cold tier + lane speedup, so the lever is **typed node flow on
  lane `Output` banks**, not the IR. Harvest list: typed flow + one-pass shaped assembly
  (perf), policy `Scheduler` + `evaluator/compiled.rs` + `enhance.rs` (stage 4), host step +
  batched function handlers (3d), `frozen.rs` + generated bundles/expected answers + graph and
  policy fuzzers (gates).
- 2026-10-01 — Stage 4 (policies) + 3d (hosts):
  - Policies: `Driver` split into `select` / `demanded` / `commit` (behaviour unchanged); the
    compiled plan (`compiled/policy/`) replays the Driver's demand recursion with scripted demand
    lists into a lazily grown schedule tree, rows partitioned by their demanded-path list after
    each select. One `Driver` per row is the row state and the exact fallback. Lane-native:
    singleton expression / assertion (shared `AssertionIr::fold`) / match blocks, and iterated
    blocks over flattened (row, instance) lanes when a static check proves no cross-instance
    reads (no unresolved reads, none touching the iteration source or the block's own writes, no
    `$root`/`$`). Tables delegate per row. Goals compile per goal set (bounded cache of 32);
    trace/extras stay on the Driver. Any lane error re-runs the row through the Driver for the
    exact error. `PolicyWorkspace::evaluate_batch`, hidden `evaluate_with_driver` (oracle).
  - Shared prepare (both engines): cached per-artifact input requirements, lazy validation paths,
    date-free entity skip — Driver ~1.9× faster on generated policies; outcomes byte-identical to
    the pre-change Driver on 7,200 generated cases (A/B dump vs `76e8cb91`).
  - Gates: `tests/compiled_policy.rs` (evaluation.toml variants + 300 generated documents from the
    harvested `policy_gen.rs`, goals derived per document; single + batch vs Driver).
  - Graph host steps: function / decision / custom nodes run through the walker's own handlers per
    row inside the replay schedule (walker context: iteration 0, `max_depth`, `$nodes` always);
    110 of 113 fixtures compiled (rest: invalid graphs, one `$`-reading cell table). Graph
    differential compares serialized errors now; `http-function.json` excluded (live HTTP,
    per-request `cf-ray` header).
  - `DecisionEngine::evaluate_batch(key, &[Variable], options)` for graphs and policies.
  - Decision: traces stay on the walker / Driver (they are the oracle and the trace engine);
    compiled traces would re-run the handlers. Walker deletion stays off (as P8 before).
  - Perf: policies single 1.01×, batch 1.22× over the (now faster) Driver — per-row input
    preparation (~36%) and delegated tables (~28%) dominate. Open levers: native policy tables,
    prefix column bindings for iterated phase scopes (avoid a store `depth_clone` per instance),
    typed node flow for graphs (graphs batch 3.79×, single 1.36×; old IR 7.66× / 3.39×).
- 2026-10-01 — Graph assembly: layers hold precomputed `Symbol` keys and apply without temporary
  objects or per-key `Arc<str>` allocations, output maps are reserved up front
  (`VariableMap::reserve`), a uniquely owned single ending map is reused with null keys removed
  (`merge_ending`-equivalent). Differential now compares serialized results (key order included).
  Perf on the 97 fixtures comparable with 3b: single 1.32×, batch 4.13× (was 3.79×); the 10 newly
  compiled host-node graphs: single 1.01×, batch 1.92×. Old IR reference: 3.39× / 7.66×.
  Bench skips `http-function.json` (live HTTP), `infinite-function.json`/`sleep-function.json`.
- 2026-10-01 — Columnar graphs (`Decision::evaluate_columns(&Columns)` → `ColumnarOutput` of typed
  `OutputColumn`s; non-compiled decisions fall back to row evaluation + flatten). Typed node flow
  inside the same engine: leaves are `Leaf::{Any, Typed(TypedCol), Picked}` (lane `Output` banks
  moved in, `Values::Scaled` binds them back without conversion), input columns enter as one
  typed record layer, per-signature program specialization (`specialize_columns`, cached by
  program id), typed `CellSet::indexed_column` (byte-keyed strings, linear small sets, scaled-int
  range probes), first-hit tables in rounds with compile-time literal outputs written straight
  into typed columns, presence-tracked leaves (columnar joins, multi-ending output, subtree
  removal by null), lazy picks/masks, binding only read leaves, shared blank scopes, static
  `$nodes.<name>.<path>` bindings, precise program opacity (`Program::opaque`) instead of
  "unkeyed site ⇒ whole row", table cells over bound columns (`CellEnv`), bitmask switch grouping,
  lazy lane faults (`exec::Fault`, static errors converted only when surfaced).
  Gate: `columnar_graphs_match_the_walker` (typed columns built from fixture variants, rows
  rebuilt from output columns vs walker, nulls/empty objects normalized) + the order-sensitive
  row differential; both clean throughout.
  Perf (`columnar_graph_throughput`, 1024 rows, type-preserving variants, walker per row vs
  columnar per row): median 4.1× → 11.2×, geomean 3.8× → 9.2×, p90 7.4× → 31×; per-row cost is
  flat from 1024 to 8192 rows.
  Calibration: old IR `program.run().columns()` on the same fixtures/inputs (scratch worktree
  test `oldcols.rs`): on the 40 fixtures it settles fully, median 70.7× / geomean 76.1×; lane on
  the same 40: median 20.0× / geomean 19.4×. (Unsettled old-IR rows are free in its numbers, so
  only fully settled fixtures compare.) Biggest remaining gaps: list/closure-heavy expressions
  (`table-collect-columns`, `product-listing-scoring`), per-row glue in tables, untyped lane cold
  paths (`Frame::set`, `Executor::cold`), schema inputs (validation materializes rows),
  loops/inputField (row-major), host nodes (QuickJS).
  Spec note: run `tests/spec.rs` in release with default features (`--all-features` switches the
  stack VM to regex-lite; debug builds hit master's overflow panics in `median([])`/empty
  templates) — release default: clean.
- 2026-10-01 — Columnar gap round (each step gated by both graph differentials, the lane tests and
  the release spec; new private fixtures in `core/engine/tests/data/compiled/`: collect tables,
  reference cells, inputField/outputPath, input schemas).
  - Tables: typed first-collect (`[]` outputs) via first-hit choice + per-rule collect batches;
    computed rule outputs exported through the lane per selection round (`First.batches`) and merged
    into typed columns per rule cell; first-bit choice for literal-only tables; list literals
    constant-folded (`LaneProgram::literal` now covers arrays) with per-call templates; text
    literals emitted as coded columns (`Values::Dict` over `Dictionary::Text`, no bytes copied per
    row); dictionary/absent/single-word fast paths in `CellSet::indexed_column`; `> a and <= b`
    style cells indexed as ranges; reference cells (`!= other.field`, closures over `$`) verified
    lazily with `$` bound to the field column instead of per-row scopes.
  - Graph: inputField expression nodes rebased onto the column path (reads resolve under the field
    path, row scopes rooted at the field subtree only when needed), outputPath as prefixed layer
    keys, `root` derived from compiled keys (statically resolved `$.x` no longer materializes rows),
    top-key subtree columns cached per data, symbol-segment object assembly, cleaning without
    clones, columnar concat when gathering across switch pieces, input schemas proven valid per
    column (`compiled/schema.rs`, port of the old IR `SchemaCheck`; unproven rows still go through
    jsonschema one row at a time), input-independent stable switch conditions evaluated once
    (`Program::stable`), word-level condition truths.
  - Lane: per-call reset of the per-site dictionary caches (stale-cache bug once dictionaries vary
    between calls), literal arrays as unpinned constants deep-copied per lane.
  Settled set vs old IR (same 40 fixtures): lane median 20.0× → 39.0×, geomean 19.4× → 44.3×
  (old IR 70.7× / 76.1×); all-fixture columnar median 12.3× → 18.1×. Now faster than the old IR:
  expression-fields (159 vs 415 ns), switch-performance (25 vs 33), switch-node (7 vs 9),
  8k (≈511 vs 650). Remaining gaps, by cause: closures over array literals and list-of-object
  inputs (`filter([...], # == true)`, `map(xs, #.f)`: items are `Dyn`, bodies run per item through
  generic ops; needs typed list registers / list columns of structs), the per-row floor of the
  table path (~12–21 ns vs 5–8: candidate masks, column building, input copies), object-heavy
  outputs (product-listing 1.4 µs vs 0.5), looped transforms and host nodes (still row-major).
- 2026-10-02 — Phase-attribution round (per-graph profiles split into input / index / select / table output /
  lane / glue / objects; scratchpad `phases.py`). Six levers, measured after each (3-run minimum, settled
  40-fixture set, lane geomean / median; old IR 76.1× / 70.7×): baseline 44.3 / 39.7 →
  (1) rule-coded table output columns (one shared code vector, per-rule dictionaries for text/number/bool;
  lane dense loads for number/bool dictionaries) 45.2 / 38.8 → (2) index: elementwise candidate AND,
  word-level bool cells, prefix-keyed small string sets 45.6 / 39.6 → (3) glue: lazy stitched leaves,
  slice-based presence, linear piece location 48.5 / 42.7 → (4) input: word-derived presence (small) →
  (5) lane: lazy builtin faults (message formatted only when surfaced; parity test vs the stack VM),
  bulk Any→Dyn loads 49.1 / 42.5; typed list inputs were tried and reverted (−3%: lists consumed by
  generic ops rebuild `Variable` arrays per read) → (6) object literals in expression entries expanded
  into per-field entries 49.3 / 42.4. Net +11% geomean. Conclusion: the remaining gap to the old IR is
  structural — every node materializes typed columns, presence and layers between nodes (≈10–40 ns per
  node per row on small graphs), while the old IR ran fused per-row code. Next lever is fusing graph
  segments (expressions + table probes) into one lane program per switch segment.
- 2026-10-05 — IR v2 ("plan"): design from an 11-agent workflow (hand-written ceilings: multi-switch 3.2 ns/row,
  traffic-violation 3.0, product-listing 11.4 vs engine 179/80/1376 and old IR 59/81/590 — the gap is engine
  overhead, not lane kernels). Implemented in `core/engine/src/compiled/plan/` (view.rs, mod.rs, run.rs):
  per-input-layout bind-once plans; typed views (leaf slot + presence mask, object masks) whose merge reproduces
  `merge_variables` (recursive / wholesale / explicit-null rules as mask algebra); flat op list (lane ops with
  site bindings resolved at bind time, table adapter, selects); switches as a lazily bound segment tree that
  shares parent slots and runs children on row selections with scattered slots (no regrouping of upstream
  columns); uniform switch conditions evaluated once; outputs captured as base slot + presence and combined
  across switch groups by mask. Tables: piece dispatch (per-column piece ids, compile-time first-hit candidate
  LUT for piece products <= 64k). Refused graphs (hosts, input/output schemas, $nodes/root reads, transforms,
  collect tables, object reads) stay on the previous engine. Gates: both differentials plus a new wide-input
  differential (randomized literal-seeded leaves, nulls, absences, unseen strings, mixed scales; row and
  columnar paths) and `plan_census`. Thin-LTO calibration (`bench.sh`, cargo-target-thin): old IR 78.0/75.4,
  pre-rebuild 52.6/44.1, now 82.4/73.4 geomean/median on the settled set; all-fixture geomean 0.82x of the
  pre-rebuild time. 68/120 graphs planned.
- 2026-10-05 (later) — Plan widening and per-row cost round (94/120 graphs planned; refused: hosts, loops,
  output schemas, whole `$nodes`, one collect-root). Earlier in the day: coded table outputs with frozen static
  dictionaries, dictionary peeling for value-only cells, batched field-less cells, typed literal slot shapes,
  bind-time folding of input-independent switch conditions, typed scatter across switch groups → 94.9 / 88.8.
  This round, each step gated by both differentials, the wide-input differential and (for lane changes) the
  release lane + spec suites:
  - Output assembly: coded gather across switch groups (dictionaries concatenated, codes offset; no text copied
    per row); `valued` per dictionary code instead of per row; well-formed text columns take validity as valued;
    scattered `truths` without densifying; u64 collect-switch keys; cached presence spreads per table.
  - Inputs: input leaves built lazily on first read (date scan only for read columns; the scan itself is a
    non-allocating `DeclaredDates::dated` walk).
  - Tables: array literals coded through per-call `Any` dictionaries (shared per rule, items are scalars or
    nested scalar arrays, never objects); literal-only collect rules assembled once per call; literal collect
    lists coded by matched-rule set (one shared array per distinct set); validity-filtered codes cached.
  - Expressions: input-independent ops (no reads, stable, not opaque) evaluated on one row and broadcast as
    constant-coded columns (scalar-only arrays shared, objects deep-cloned per row).
  - Objects: per-row objects built through a precomputed shape (`VariableMap::from_shape`) for complete rows.
  - Schema proof: per-leaf presence and typed validity as bitmasks.
  - Lane: Any cells borrowed for builtin args; byte-level text equality against string constants; borrowed
    `Equal` for typed vs dynamic operands (bits result); `contains(array, text)` over borrowed dynamic arrays;
    number scalings cached per scale (bench inputs alternate scales — one cached scaling rebuilt each row);
    `specialize_bound` — cell programs specialize through the bound columns, so a rebound `$` gets the reference
    column's kind instead of `Dyn` (realtime-fraud 161 → 136 ns).
  Settled set (thin LTO, 3-run minimum): 94.9 / 88.8 → 120.2 / 109.0 geomean / median (old IR 78.0 / 75.4);
  all fixtures 1.16× faster in geomean than the start of the round. Biggest: smart-financial 70 → 16 ns,
  switch-performance-2 36 → 6, clinical-pathway 279 → 131, multi-switch 115 → 70, insurance-underwriting
  604 → 381, product-listing 743 → 521, table-collect-columns 52 → 31, empty-column 11 → 7. Still behind the old
  IR: realtime-fraud 136 vs 98, municipal-permit 113 vs 86, set-fee 108 vs 85, SLA 111 vs 89, multi-switch
  70 vs 59. Fixed cost per call is 2–16 µs (a third of it `libsystem_malloc` at 16 rows: lane `Output` resets,
  table column building, mask arenas). Graphs dominated by failing input-schema rows (medication-dosage,
  immigration, flash-sale; 1–1.4 µs/row) spend ~80% in the shared walker validation path (`nodes/context.rs`:
  jsonschema `iter_errors` + `strip_nulls`), left as is.
- 2026-10-05 (evening) — Execution-model test and list columns. An isolated harness (scratchpad `models/`, the real lane VM
  vs copy/view block interpreters, eager whole-column kernels and per-row closures, 8 expressions, full batch and 47% row
  selections) showed no execution model wins overall; copy-vs-view loads differ by 0.1–0.9 ns/row; the lane VM's slow
  spots were implementation: list `contains` (45 vs 6.5 ns), list `sum` (19 vs 3.8), masked text equality (6.1 vs 2.7),
  null-comparison faults (two heap strings each). Fixed in the lane: list contains/reducers in place, inline byte equality
  for any mask and typed on selections, faults stored lazily in outputs, branchless masked scale check — harness now
  4.4 / 4.2 / 2.7 / 8.3 / 9.3 ns for compare / and-chain / masked eq / sum / contains (best prototype 4.3 / 4.9 / 2.7 /
  4.0 / 6.8); engine bench flat (+0.2%) because the bench built arrays as `Any` and tables use their own kernels.
  Arrow-list mode added to the bench and both differentials (`BENCH_LISTS=1`; differentials always run both modes): list
  columns end to end — `Store::List` gathers, schema proof over lists, `Kind::List` registers filled from list columns,
  `x ?? []` keeps the list type, list equality/contains without materializing. List mode vs pre-built `Any` arrays:
  customer-eligibility 1472 → 568 (Any 587), realtime-fraud 152 → 121 (115), clinical-pathway 162 → 142 (132),
  multi-switch 124 → 101 (70). Measured and not pursued: text register views (arena copies 0–1.1% of a call),
  converting decimal input columns once (no gain, pass-through got slower). Rejected earlier in the day: grouping rows by
  coded inputs (slower everywhere), one isolated program for reference cells in first-hit tables and an in-program
  first-hit rule loop (both lost laziness / load sharing). Kept: combined isolated program for field-less cells, planned
  allocation (−20% allocations per call). Settled set 125.4 / 111.0.
- 2026-10-05 (night) — Targeted round on graphs trailing the old IR, each found by profile and kept only if measured:
  coded gathers keep dictionary columns coded (set-fee 99 → 75); template interpolation `${…}` no longer blocks
  expanding object literals into typed per-field entries (clinical-treatment-protocol 390 → 138); repeated top-level
  field-vs-string equality shares one register across entries (municipal-permit 100 → 89); literal `null` entries get a
  scalar null shape so explicit nulls over objects merge by masks (legacy-plan 134 → 73); dictionary columns classify
  their dictionary through the indexed fast paths. Tried and reverted: decimal input views for lanes/tables (no gain),
  an `I64` classify fast path (bench encodes integers as decimals; unmeasurable). Corpus census (1 s per fixture): no
  kernel above 8% — classify 7.6%, memmove 6.9%, allocator ~15%. Settled set 132.1 / 126.5 (old IR 78.0 / 75.4), faster
  than the old IR on 34 of 40; still behind: SLA 110 vs 89, realtime-fraud 117 vs 98, multi-switch 68 vs 59.
- 2026-10-06 — Hand-off census for "pass registers between lane ops": of 803 planned reads, 60% come from input
  columns, 27% from table outputs, 9% lane→table and <1% lane→lane, so register-to-register hand-off has no targets.
  What the hand-offs do allow: lane ops whose reads are all coded (table outputs) or boolean, with a stable and
  non-opaque program, key each row by its code tuple (dense LUT, product of dictionary sizes ≤ 4096, at least 16 rows,
  bail when distinct > rows/4), evaluate only one representative row per key, and return coded outputs (codes = key
  ids, dictionary = representative results; objects are deep-cloned per row instead). Faults fan out to every row of
  the key. The cycle is bounded by rule counts, not input variety, so it is not a bench artifact. Covers switch
  conditions too. Columnar differential gains a cycled-rows mode (variants ×4) because the plain variant sets never
  reach the path. application-risk 239 → 68, realtime-fraud 117 → 99, airline-loyalty 39 → 31. Settled set
  135.7 / 134.8 (old IR 78.0 / 75.4), faster on 35 of 40; behind: SLA 111 vs 89, multi-switch 69 vs 59,
  realtime-fraud 99 vs 98, municipal-permit 88 vs 86.
- 2026-10-06 (overnight exploration, baseline `ef9e8532` recorded in LAST_PERF_COMMIT.md) — Each experiment gated by both
  graph differentials (plus the wide-input one, the lane tests and the decimal unit tests where the lane changed),
  measured with a 5-run-minimum full bench, kept only when it improved. Kept:
  hashed keys for wide code-tuple spaces; keyed first-hit tables over two or more coded/boolean reads; literal collect
  tables keyed by matched-rule set (key hints feed keyed lane ops); constant object/computed-constant cells as literals;
  direct gathers for rules whose outputs are only literals or plain column reads; set-bit rule spreading; composed
  first-rule LUT; shared per-row failure messages; constant-argument builtins evaluated once per frame (`date('now')`
  is therefore one instant per frame); `bool()` over booleans; boxed `contains`/`len`; `string(null)` reuse and str
  joins without revalidation; path-only expression entries alias the input view (product-listing 439 → 71 ns/row);
  static-key object literals built from one shape; cached nested shapes for table output objects; precompiled chrono
  formats; select over numeric columns built directly; schema checks resolve columns once.
  Reverted (no gain or slower): dictionary-encoding text lane outputs, exact scaled division emulating rust_decimal
  (correct, but non-terminating quotients cost more), typed list registers for array literals (dynamic-tarrif 173 →
  282), branchless/flag variants of small table loops, Rc mask builders, chunked key loops, dense word-wise text
  equality (memcmp calls lose to the byte loop), prefix-masked vector arithmetic, group-wise assembly gather.
  Allocation census (temporary counting allocator): 30–600 allocations per call, ~25–40 per table op; fixed per-call
  cost is 20–35% of the smallest graphs at 1024 rows. Settled set 182 / 158 (old IR 78 / 75), faster on 38 of 40;
  still behind: SLA 96 vs 89, multi-switch 66 vs 59.
- 2026-10-06 (exploration close) — Two more kept: `path ?? literal` entries become a typed coalesce op (SLA 97 → 89, now
  level with the old IR), and switch children are cached by condition bitset instead of building and comparing outcome
  id lists (switch-performance-2 5.3 → 4.2, multi-switch 67 → 61). Reverted: plan-level `path == literal` entries and
  switch conditions as presence/equality masks (both slower; not root-caused), single-input dictionary dispatch,
  length-first text pieces. Final A/B in one sitting with the decimal bench (baseline `ef9e8532` sources vs HEAD):
  settled 136.7 / 135.7 → 187.2 / 165.4 (old IR 78.0 / 75.4), faster than the old IR on 36 → 39 of 40; all 107
  fixtures take 0.78× the time (geomean), none slower. Only multi-switch still trails (61 vs 59).
