# Python Rules Engine

**Business logic humans can read and machines can run.** One copy of your rules: the owner reads it, every system runs it.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![PyPI](https://img.shields.io/pypi/v/zen-engine.svg)](https://pypi.org/project/zen-engine/)

<img width="1280" alt="GoRules ZEN Engine" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/hero.png">

ZEN Engine is a cross-platform, open-source [Business Rules Engine (BRE)](https://gorules.io) written in **Rust** with native **Python** bindings, alongside Node.js, Go, Java, Kotlin and .NET. Decisions evaluate in microseconds, run identically on every platform, and are stored as portable JSON. Loading the JSON is up to you: file system, database or service call.

Try it in the free [Online Editor](https://editor.gorules.io) with a built-in simulator, or embed the open-source React [JDM Editor](https://github.com/gorules/jdm-editor) in your own product. Learn more about the [Python rules engine](https://gorules.io/open-source/python-rules-engine) on the GoRules website.

## Rules that read like sentences

Conditions are written the way the business says them, in the ZEN Expression Language. The developer view is one toggle away, and the two can never drift apart: there is only one source of truth, and this engine runs it.

<img width="1280" alt="Readable rules" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/tables.png">

## Rules as graphs, or as documents

Model a decision on a visual canvas of decision tables, switches, expressions, functions and reusable sub-decisions. Or write it as a policy document with prose, typed data models and tables. Both compile to the same engine and return the same answers.

<img width="1280" alt="Graphs and documents" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/graphs-docs.png">

To go deeper, see the [Python SDK documentation](https://docs.gorules.io/developers/sdks/python), the [decision graph guide](https://docs.gorules.io/learn/authoring/decision-graphs) and the [ZEN Expression Language](https://docs.gorules.io/learn/zen-language/syntax) reference.

## What's new in 2.0

Version 2.0 is the first stable release of the new engine line:

- **Policy documents**: model decisions as readable documents with typed data models, expressions, decision tables, match blocks and assertions. Policies compile to the same engine as graphs and return the same answers.
- **Workspace analysis**: static type checking across policies and graphs. Type flow, exhaustiveness checking, write-conflict detection and precise diagnostics, all available before anything runs.
- **Per-column collect**: decision table output columns can collect across all matching rows (`tags[]`) while the rest of the table stays first-match.
- **Pre-compiled engine**: decisions are parsed and compiled once at load; evaluation is allocation-light and repeat-safe.
- **Hardened runtime**: out-of-range numbers, arithmetic overflow and malformed inputs return errors or nulls instead of crashing the process.
- **Unified bindings**: configurable loaders, batch evaluation and consistent error envelopes across Node.js, Python, Go and FFI consumers.

## Installation

```bash
pip install zen-engine
```

Prebuilt wheels ship for Linux, macOS and Windows; no Rust toolchain required.

## Quickstart

```python
import zen

with open("./jdm_graph.json", "r") as f:
    content = f.read()

engine = zen.ZenEngine()

decision = engine.create_decision(content)
result = decision.evaluate({"input": 15})
print(result)
```

### Loaders

For more advanced use cases where you want to load multiple decisions and reuse them across evaluations you can build loaders. When `engine.evaluate` is invoked it calls the loader with a key, expecting the content of the JDM decision graph in return.

```python
import zen

def loader(key):
    with open("./jdm_directory/" + key, "r") as f:
        return f.read()

engine = zen.ZenEngine({"loader": loader})
result = engine.evaluate("jdm_graph1.json", {"input": 5})
print(result)
```

The same pattern works for loading from a REST API, S3, a database, or anywhere else. Full guides, including multi-decision graphs and batch evaluation, are in the [Python SDK documentation](https://docs.gorules.io/developers/sdks/python).

## Other platforms

* **Node.js** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/nodejs) | [Documentation](https://docs.gorules.io/developers/sdks/nodejs) | [npm](https://www.npmjs.com/package/@gorules/zen-engine)
* **Python** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/python) | [Documentation](https://docs.gorules.io/developers/sdks/python) | [PyPI](https://pypi.org/project/zen-engine/)
* **Go** - [GitHub](https://github.com/gorules/zen-go) | [Documentation](https://docs.gorules.io/developers/sdks/go)
* **Java / Kotlin** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/uniffi) | [Documentation](https://docs.gorules.io/developers/sdks/java) | [Maven Central](https://mvnrepository.com/artifact/io.gorules/zen-engine)
* **.NET** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/uniffi) | [Documentation](https://docs.gorules.io/developers/sdks/csharp) | [NuGet](https://www.nuget.org/packages/GoRules.ZenEngine)
* **Rust (Core)** - [GitHub](https://github.com/gorules/zen) | [Documentation](https://docs.gorules.io/developers/sdks/rust) | [crates.io](https://crates.io/crates/zen-engine)

## The GoRules platform

The engine is open at the core; [GoRules](https://gorules.io) is the platform around it. Managed cloud, self-hosted, or embedded with no network hop. SOC 2 Type II.

### AI that builds rules, and stays reviewable

An AI copilot and MCP server that edits rules, runs tests and explains decisions. It never deploys. Releases stay with your reviewers.

<img width="800" alt="GoRules AI" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/ai.png">

### Promote like a release, run like a binary

A release moves from testing to staging to production untouched. Approvals, instant rollback, and a paper trail for every change.

<img width="800" alt="Governance" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/governance.png">

### Prove it before it ships

Scenario suites run on every change, coverage is measured against decision paths, and every answer comes with a replayable trace.

<img width="800" alt="Testing" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/tests.png">

## Support matrix

| Arch            | Python             |
|:----------------|:-------------------|
| linux-x64-gnu   | :heavy_check_mark: |
| linux-arm64-gnu | :heavy_check_mark: |
| darwin-x64      | :heavy_check_mark: |
| darwin-arm64    | :heavy_check_mark: |
| win32-x64-msvc  | :heavy_check_mark: |

We do not support linux-musl currently.

## Contribution

The JDM standard is growing and we need to keep tight control over its development and roadmap, as a number of companies use GoRules ZEN Engine and GoRules BRMS. For this reason we can't accept code contributions at this moment, apart from help with documentation and additional tests.

## License

[MIT License](https://opensource.org/licenses/MIT)
