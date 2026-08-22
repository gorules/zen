# Rust Rules Engine

**Business logic humans can read and machines can run.** One copy of your rules: the owner reads it, every system runs it.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![crates.io](https://img.shields.io/crates/v/zen-engine.svg)](https://crates.io/crates/zen-engine)

<img width="1280" alt="GoRules ZEN Engine" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/hero.png">

ZEN Engine is a cross-platform, open-source [Business Rules Engine (BRE)](https://gorules.io) written in **Rust**. This crate is the core: the same engine that powers the Node.js, Python, Go, Java, Kotlin and .NET bindings, available with zero FFI overhead. Decisions evaluate in microseconds and are stored as portable JSON. Loading the JSON is up to you: file system, database or service call.

Try it in the free [Online Editor](https://editor.gorules.io) with a built-in simulator, or embed the open-source React [JDM Editor](https://github.com/gorules/jdm-editor) in your own product. Learn more about the [Rust rules engine](https://gorules.io/open-source/rust-rules-engine) on the GoRules website.

## Rules that read like sentences

Conditions are written the way the business says them, in the ZEN Expression Language. The developer view is one toggle away, and the two can never drift apart: there is only one source of truth, and this engine runs it.

<img width="1280" alt="Readable rules" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/tables.png">

## Rules as graphs, or as documents

Model a decision on a visual canvas of decision tables, switches, expressions, functions and reusable sub-decisions. Or write it as a policy document with prose, typed data models and tables. Both compile to the same engine and return the same answers.

<img width="1280" alt="Graphs and documents" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/graphs-docs.png">

To go deeper, see the [Rust SDK documentation](https://docs.gorules.io/developers/sdks/rust), the [decision graph guide](https://docs.gorules.io/learn/authoring/decision-graphs) and the [ZEN Expression Language](https://docs.gorules.io/learn/zen-language/syntax) reference.

## Installation

```toml
[dependencies]
zen-engine = "2"
```

> **Upgrading from 0.x?** `arbitrary_precision` is no longer a default feature. If you rely on arbitrary-precision number handling, enable it explicitly: `zen-engine = { version = "2", features = ["arbitrary_precision"] }`. Language bindings are unaffected.

## Quickstart

```rust
use zen_engine::DecisionEngine;
use zen_engine::model::DecisionContent;
use serde_json::json;

#[tokio::main]
async fn main() {
    let decision_content: DecisionContent =
        serde_json::from_str(include_str!("./pricing-rules.json")).unwrap();

    let engine = DecisionEngine::default();
    let decision = engine.create_decision(decision_content.into()).unwrap();

    let response = decision.evaluate(json!({
        "customer": { "tier": "gold", "yearsActive": 3 },
        "order": { "subtotal": 150, "items": 5 }
    }).into()).await.unwrap();

    println!("{}", response.result);
    // => {"discount":0.15,"freeShipping":true}
}
```

### Loaders

Attach a loader to serve decisions by key. Build one declaratively from `LoaderConfig` (`Static`, `Filesystem`, `Zip`), or construct the loader structs in `zen_engine::loader` directly. With a configuration, decisions are pre-loaded and pre-compiled for faster evaluations.

```rust
use zen_engine::DecisionEngine;
use zen_engine::loader::LoaderConfig;
use serde_json::json;

#[tokio::main]
async fn main() {
    let loader = LoaderConfig::Filesystem { path: "./rules".to_string() }
        .into_loader()
        .unwrap();
    let engine = DecisionEngine::default().with_loader(loader);

    let response = engine.evaluate("pricing.json", json!({ "amount": 100 }).into()).await.unwrap();
    println!("{}", response.result);
}
```

Custom backends (REST API, S3, database) implement the `DecisionLoader` trait. Full guides, including all loader variants and expression evaluation, are in the [Rust SDK documentation](https://docs.gorules.io/developers/sdks/rust).

## Other platforms

* **Node.js** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/nodejs) | [Documentation](https://docs.gorules.io/developers/sdks/nodejs) | [npm](https://www.npmjs.com/package/@gorules/zen-engine)
* **Python** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/python) | [Documentation](https://docs.gorules.io/developers/sdks/python) | [PyPI](https://pypi.org/project/zen-engine/)
* **Go** - [GitHub](https://github.com/gorules/zen-go) | [Documentation](https://docs.gorules.io/developers/sdks/go)
* **Java / Kotlin** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/uniffi) | [Documentation](https://docs.gorules.io/developers/sdks/java) | [Maven Central](https://central.sonatype.com/artifact/io.gorules/zen-engine)
* **.NET** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/uniffi) | [Documentation](https://docs.gorules.io/developers/sdks/csharp) | [NuGet](https://www.nuget.org/packages/GoRules.ZenEngine)
* **Swift (iOS)** - [GitHub](https://github.com/gorules/zen-ios) | [Documentation](https://docs.gorules.io/developers/sdks/ios)

## The GoRules platform

The engine is open at the core; [GoRules](https://gorules.io) is the platform around it. Managed cloud, self-hosted, or embedded with no network hop. SOC 2 Type II.

## Contribution

The JDM standard is growing and we need to keep tight control over its development and roadmap, as a number of companies use GoRules ZEN Engine and GoRules BRMS. For this reason we can't accept code contributions at this moment, apart from help with documentation and additional tests.

## License

[MIT License](https://opensource.org/licenses/MIT)
