# .NET Rules Engine

**Business logic humans can read and machines can run.** One copy of your rules: the owner reads it, every system runs it.

<img width="1280" alt="GoRules ZEN Engine" src="https://raw.githubusercontent.com/gorules/zen/master/.github/images/hero.png">

ZEN Engine is a cross-platform, open-source [Business Rules Engine (BRE)](https://gorules.io) written in **Rust** with native **.NET** bindings, alongside Node.js, Python, Go, Java and Kotlin. Decisions evaluate in microseconds, run identically on every platform, and are stored as portable JSON Decision Models (JDM). Loading the JSON is up to you: file system, database or service call.

Try it in the free [Online Editor](https://editor.gorules.io) with a built-in simulator, or embed the open-source React [JDM Editor](https://github.com/gorules/jdm-editor) in your own product. Learn more about the [C# rules engine](https://gorules.io/open-source/csharp-rules-engine) on the GoRules website.

## Installation

```bash
dotnet add package GoRules.ZenEngine
```

## Quick Start

```csharp
using GoRules.ZenEngine;

// Create an engine and evaluate
var engine = new ZenEngine(loader: null, customNode: null);
var decision = engine.CreateDecision(new JsonBuffer(File.ReadAllBytes("my-decision.json")));
var context = new JsonBuffer("""{"input": 42}""");
var response = await decision.Evaluate(context, null);
Console.WriteLine(response.Result);

```

## Loader Configurations

The `loader` argument accepts either a callback (`ZenLoader.Callback`) or a loader configuration
of a known type. With a configuration, decisions are pre-loaded and pre-compiled at engine
creation for faster evaluations:

```csharp
var fsEngine = new ZenEngine(loader: new ZenLoader.Filesystem("decisions"));
var zipEngine = new ZenEngine(loader: new ZenLoader.Zip(File.ReadAllBytes("decisions.zip")));
var cbEngine = new ZenEngine(loader: new ZenLoader.Callback(new FileLoader()));
```

## Features

- **Decision Tables** - Rule tables with first/collect hit policies
- **Expression Language** - Built-in ZEN expression language with functions like `sum()`, `filter()`, `map()`
- **Custom Nodes** - Extend the engine with custom node handlers
- **Tracing** - Full execution trace for debugging and auditing
- **Cross-platform** - Native libraries for Windows (x64), macOS (x64/ARM), Linux (x64/ARM)

## Tracing

Enable tracing to inspect the execution of each node:

```csharp
var options = new ZenEvaluateOptions(MaxDepth: null, Trace: true);
var decided = await decision.Evaluate(context, options);

foreach (var (nodeId, trace) in decided.Trace!)
{
    Console.WriteLine($"{trace.Name}: {trace.Output}");
}
```

## Expression Evaluation

Evaluate expressions directly without a decision file:

```csharp
// One-off evaluation
var result = ZenUniffiMethods.EvaluateExpression(
    "sum(items) * multiplier",
    new JsonBuffer("{\"items\": [10, 20, 30], \"multiplier\": 2}")
);

// Compiled expression (reusable, better performance)
var expr = ZenExpression.Compile("a + b * 2");
var output = expr.Evaluate(new JsonBuffer("{\"a\": 1, \"b\": 10}"));
Console.WriteLine($"output: {output}");
expr.Dispose();
```

## Custom Nodes

Extend the engine with custom logic:

```csharp

using var myEngine = new ZenEngine(loader: new ZenLoader.Callback(new FileLoader()), customNode: new MyCustomNode());
var myResponse = await myEngine.Evaluate("custom.json", context, options);
Console.WriteLine(myResponse.Result);

// Custom node handler
class MyCustomNode : ZenCustomNodeCallback
{
    public Task<ZenEngineHandlerResponse> Handle(ZenEngineHandlerRequest request) =>
        Task.FromResult(new ZenEngineHandlerResponse(
            Output: new JsonBuffer("""{"result": "custom"}"""),
            TraceData: null
        ));
}

// Implement a loader to resolve decision files
class FileLoader : ZenDecisionLoaderCallback
{
    public Task<JsonBuffer?> Load(string key) =>
        Task.FromResult<JsonBuffer?>(new JsonBuffer(File.ReadAllBytes(key)));
}

```

## Other platforms

* **Node.js** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/nodejs) | [Documentation](https://docs.gorules.io/developers/sdks/nodejs) | [npm](https://www.npmjs.com/package/@gorules/zen-engine)
* **Python** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/python) | [Documentation](https://docs.gorules.io/developers/sdks/python) | [PyPI](https://pypi.org/project/zen-engine/)
* **Go** - [GitHub](https://github.com/gorules/zen-go) | [Documentation](https://docs.gorules.io/developers/sdks/go)
* **Java / Kotlin** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/uniffi) | [Documentation](https://docs.gorules.io/developers/sdks/java) | [Maven Central](https://mvnrepository.com/artifact/io.gorules/zen-engine)
* **.NET** - [GitHub](https://github.com/gorules/zen/tree/master/bindings/uniffi) | [Documentation](https://docs.gorules.io/developers/sdks/csharp) | [NuGet](https://www.nuget.org/packages/GoRules.ZenEngine)
* **Rust (Core)** - [GitHub](https://github.com/gorules/zen) | [Documentation](https://docs.gorules.io/developers/sdks/rust) | [crates.io](https://crates.io/crates/zen-engine)

## Links

- [GoRules](https://gorules.io) - the platform around the open-source engine: managed cloud, self-hosted, or embedded. SOC 2 Type II.
- [.NET SDK Documentation](https://docs.gorules.io/developers/sdks/csharp)
- [GitHub Repository](https://github.com/gorules/zen)
- [JDM Editor](https://editor.gorules.io)

## License

[MIT License](https://opensource.org/licenses/MIT)
