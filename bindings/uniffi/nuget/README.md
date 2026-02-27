# GoRules.ZenEngine

Open-source Business Rules Engine for .NET. Execute JSON Decision Models (JDM) with native performance powered by Rust.

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
Console.WriteLine(response.result);

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
var decided = await decision.Evaluate(context, options);

foreach (var (nodeId, trace) in decided.trace!)
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

using var myEngine = new ZenEngine(loader: new FileLoader(), customNode: new MyCustomNode());
var myResponse = await myEngine.Evaluate("custom.json", context, options);
Console.WriteLine(myResponse.result); 

// Custom node handler
class MyCustomNode : ZenCustomNodeCallback
{
    public Task<ZenEngineHandlerResponse> Handle(ZenEngineHandlerRequest request) =>
        Task.FromResult(new ZenEngineHandlerResponse(
            output: new JsonBuffer("""{"result": "custom"}"""),
            traceData: null
        ));
}

// Implement a loader to resolve decision files
class FileLoader : ZenDecisionLoaderCallback
{
    public Task<JsonBuffer?> Load(string key) =>
        Task.FromResult<JsonBuffer?>(new JsonBuffer(File.ReadAllBytes(key)));
}

```

## Links

- [GitHub Repository](https://github.com/gorules/zen)
- [GoRules Documentation](https://docs.gorules.io/developers/sdks/csharp)
- [JDM Editor](https://editor.gorules.io)
