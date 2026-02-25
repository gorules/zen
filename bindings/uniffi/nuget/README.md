# GoRules.ZenEngine

Open-source Business Rules Engine for .NET. Execute JSON Decision Models (JDM) with native performance powered by Rust.

## Installation

```bash
dotnet add package GoRules.ZenEngine
```

## Quick Start

```csharp
using GoRules.ZenEngine;

// Create engine and evaluate
var engine = new ZenEngine(loader: new FileLoader(), customNode: null);
var context = new JsonBuffer("{\"input\": 42}");
var response = await engine.Evaluate("my-decision.json", context, null);
Console.WriteLine(response.result.ToString());

// Implement a loader to resolve decision files
class FileLoader : ZenDecisionLoaderCallback
{
    public Task<JsonBuffer?> Load(string key)
    {
        var bytes = File.ReadAllBytes(key);
        return Task.FromResult<JsonBuffer?>(new JsonBuffer(bytes));
    }
}
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
var options = new ZenEvaluateOptions(maxDepth: null, trace: true);
var response = await engine.Evaluate("decision.json", context, options);

foreach (var (nodeId, trace) in response.trace!)
{
    Console.WriteLine($"{trace.name}: {trace.output}");
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
class MyCustomNode : ZenCustomNodeCallback
{
    public Task<ZenEngineHandlerResponse> Handle(ZenEngineHandlerRequest request)
    {
        var output = new JsonBuffer("{\"result\": \"custom\"}");
        return Task.FromResult(new ZenEngineHandlerResponse(
            output: output,
            traceData: null
        ));
    }
}

var engine = new ZenEngine(loader: new FileLoader(), customNode: new MyCustomNode());
```

## Links

- [GitHub Repository](https://github.com/gorules/zen)
- [GoRules Documentation](https://gorules.mintlify.app/developers/sdks/csharp)
- [JDM Editor](https://editor.gorules.io)
