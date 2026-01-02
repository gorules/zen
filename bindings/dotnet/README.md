# GoRules.Zen - C# Bindings for Zen Rules Engine

C# P/Invoke bindings for the [Zen Rules Engine](https://github.com/gorules/zen), providing high-performance business rules evaluation for .NET applications.

## Overview

This package provides native C# bindings to the Zen Rules Engine via P/Invoke, allowing you to:

- Evaluate expressions with a powerful expression language
- Execute business rules defined in JSON decision graphs
- Use templates for dynamic string rendering
- Integrate custom logic via callbacks

### Why P/Invoke Instead of UniFFI?

The Zen project uses [UniFFI](https://mozilla.github.io/uniffi-rs/) for multi-language bindings (Kotlin, Java, Swift). However, UniFFI's C# support has limitations that prevent it from working correctly with this codebase. This P/Invoke implementation provides a direct, reliable alternative by:

1. Using the existing C FFI bindings (`bindings/c/`)
2. Wrapping them with idiomatic C# classes
3. Handling memory management and marshalling automatically

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Your C# Application                      │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│              GoRules.Zen (C# Wrapper)                       │
│  • ZenEngine, ZenDecision, ZenExpression                    │
│  • Automatic memory management                              │
│  • Type-safe API with generics                              │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼ P/Invoke
┌─────────────────────────────────────────────────────────────┐
│              libzen_ffi.so / zen_ffi.dll                    │
│  • C FFI exports with extern "C"                            │
│  • cbindgen-generated headers                               │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   Zen Core (Rust)                           │
│  • zen-engine: Decision graph execution                     │
│  • zen-expression: Expression language VM                   │
│  • zen-template: Template rendering                         │
└─────────────────────────────────────────────────────────────┘
```

## Building

### Prerequisites

- .NET SDK 8.0+ (tested with .NET 10)
- Rust toolchain (for building native library)

### Step 1: Build the Native Library

```bash
# From the repository root
cd bindings/c

# Build without Go feature (required for .NET)
cargo build --release --no-default-features

# Output locations:
# - Linux:   ../../target/release/libzen_ffi.so
# - macOS:   ../../target/release/libzen_ffi.dylib
# - Windows: ../../target/release/zen_ffi.dll
```

### Step 2: Copy Native Library to Runtime Folder

```bash
# Linux x64
mkdir -p bindings/dotnet/runtimes/linux-x64/native
cp target/release/libzen_ffi.so bindings/dotnet/runtimes/linux-x64/native/

# macOS x64
mkdir -p bindings/dotnet/runtimes/osx-x64/native
cp target/release/libzen_ffi.dylib bindings/dotnet/runtimes/osx-x64/native/

# macOS ARM64
mkdir -p bindings/dotnet/runtimes/osx-arm64/native
cp target/release/libzen_ffi.dylib bindings/dotnet/runtimes/osx-arm64/native/

# Windows x64
mkdir -p bindings/dotnet/runtimes/win-x64/native
cp target/release/zen_ffi.dll bindings/dotnet/runtimes/win-x64/native/
```

### Step 3: Build the C# Library

```bash
cd bindings/dotnet
dotnet build
```

### Step 4: Run Tests

```bash
dotnet test
```

## Usage

### Expression Evaluation

```csharp
using GoRules.Zen;

// Simple arithmetic
var result = ZenExpression.Evaluate("a + b", """{"a": 10, "b": 20}""");
// result = "30"

// String operations
var greeting = ZenExpression.Evaluate(
    "firstName + \" \" + lastName",
    """{"firstName": "John", "lastName": "Doe"}"""
);
// greeting = "\"John Doe\""

// Array functions
var max = ZenExpression.Evaluate("max(scores)", """{"scores": [85, 92, 78]}""");
// max = "92"

// Ternary expressions
var status = ZenExpression.Evaluate(
    "age >= 18 ? \"adult\" : \"minor\"",
    """{"age": 21}"""
);
// status = "\"adult\""
```

### Unary (Boolean) Expressions

Unary expressions compare a value against an expression. The context value is accessed via `$`:

```csharp
using GoRules.Zen;

// Greater than
var isAdult = ZenExpression.EvaluateUnary("> 18", """{"$": 21}""");
// isAdult = true

// Equality
var isActive = ZenExpression.EvaluateUnary("== \"active\"", """{"$": "active"}""");
// isActive = true

// In array
var isValid = ZenExpression.EvaluateUnary(
    "in [\"pending\", \"active\"]",
    """{"$": "active"}"""
);
// isValid = true
```

### Template Rendering

```csharp
using GoRules.Zen;

var result = ZenExpression.RenderTemplate(
    "Hello {{ name }}! You have {{ count }} messages.",
    """{"name": "Alice", "count": 5}"""
);
// result = "\"Hello Alice! You have 5 messages.\""
```

### Typed Context and Results

Use generics to work with strongly-typed objects:

```csharp
using GoRules.Zen;

var context = new { a = 15, b = 25 };
int result = ZenExpression.Evaluate<object, int>("a + b", context);
// result = 40
```

### Decision Evaluation

```csharp
using GoRules.Zen;

// Create engine
using var engine = new ZenEngine();

// Load decision from JSON
string decisionJson = File.ReadAllText("my-decision.json");
using var decision = engine.CreateDecision(decisionJson);

// Evaluate with context
var result = decision.Evaluate("""{"customer": {"age": 25}}""");

// With tracing enabled
var options = new EvaluationOptions { Trace = true, MaxDepth = 10 };
var tracedResult = decision.Evaluate("""{"customer": {"age": 25}}""", options);
```

### Decision Loader Callback

Load decisions dynamically from any source:

```csharp
using GoRules.Zen;

using var engine = new ZenEngine(
    loader: key =>
    {
        // Load from file system
        var path = $"decisions/{key}.json";
        if (File.Exists(path))
            return File.ReadAllText(path);

        // Or load from database, HTTP, etc.
        return null; // Return null if not found
    }
);

// The loader is called automatically
var result = engine.Evaluate("pricing-rules", """{"product": "widget"}""");
```

### Custom Node Handler

Handle custom node types in decision graphs:

```csharp
using GoRules.Zen;
using System.Text.Json;

using var engine = new ZenEngine(
    loader: key => File.ReadAllText($"decisions/{key}.json"),
    customNode: request =>
    {
        var doc = JsonDocument.Parse(request);
        var nodeKind = doc.RootElement
            .GetProperty("node")
            .GetProperty("kind")
            .GetString();

        return nodeKind switch
        {
            "httpCall" => HandleHttpCall(doc),
            "dbLookup" => HandleDbLookup(doc),
            _ => throw new NotSupportedException($"Unknown node: {nodeKind}")
        };
    }
);
```

### Error Handling

```csharp
using GoRules.Zen;
using GoRules.Zen.Interop;

try
{
    var result = ZenExpression.Evaluate("invalid !!!", "{}");
}
catch (ZenException ex)
{
    Console.WriteLine($"Error Code: {ex.ErrorCode}");
    Console.WriteLine($"Details: {ex.Details}");

    // Handle specific errors
    if (ex.ErrorCode == ZenErrorCode.EvaluationError)
    {
        // Handle evaluation error
    }
}
```

## API Reference

### ZenExpression (Static Class)

| Method | Description |
|--------|-------------|
| `Evaluate(expression, context)` | Evaluate expression, returns JSON string |
| `Evaluate<TContext, TResult>(expression, context)` | Typed evaluation |
| `EvaluateUnary(expression, context)` | Evaluate boolean expression |
| `EvaluateUnary<TContext>(expression, context)` | Typed unary evaluation |
| `RenderTemplate(template, context)` | Render template string |
| `RenderTemplate<TContext>(template, context)` | Typed template rendering |

### ZenEngine

| Method | Description |
|--------|-------------|
| `ZenEngine()` | Create engine without callbacks |
| `ZenEngine(loader, customNode)` | Create engine with callbacks |
| `CreateDecision(json)` | Create decision from JSON string |
| `GetDecision(key)` | Get decision via loader callback |
| `Evaluate(key, context, options)` | Evaluate decision by key |
| `Evaluate<TContext, TResult>(...)` | Typed evaluation |
| `Dispose()` | Free native resources |

### ZenDecision

| Method | Description |
|--------|-------------|
| `Evaluate(context, options)` | Evaluate the decision |
| `Evaluate<TContext, TResult>(...)` | Typed evaluation |
| `Dispose()` | Free native resources |

### EvaluationOptions

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `Trace` | bool | false | Enable execution trace |
| `MaxDepth` | byte | 5 | Maximum recursion depth |

### Error Codes (ZenErrorCode)

| Code | Description |
|------|-------------|
| `Success` | No error |
| `InvalidArgument` | Invalid argument provided |
| `JsonSerializationFailed` | JSON serialization error |
| `JsonDeserializationFailed` | JSON parsing error |
| `IsolateError` | Expression evaluation error |
| `EvaluationError` | Decision evaluation error |
| `LoaderKeyNotFound` | Decision key not found |
| `LoaderInternalError` | Loader callback error |
| `TemplateEngineError` | Template rendering error |

## Project Structure

```
bindings/dotnet/
├── GoRules.Zen.sln              # Solution file
├── GoRules.Zen.csproj           # Main library project
├── ZenEngine.cs                 # High-level wrapper classes
├── ZenEngine.Interop.cs         # P/Invoke definitions
├── README.md                    # This file
├── runtimes/                    # Native libraries (per-platform)
│   ├── linux-x64/native/
│   ├── osx-x64/native/
│   ├── osx-arm64/native/
│   └── win-x64/native/
└── GoRules.Zen.Tests/           # Unit tests
    ├── ExpressionTests.cs
    ├── UnaryExpressionTests.cs
    ├── TemplateTests.cs
    └── EngineTests.cs
```

## Platform Support

| Platform | Architecture | Status |
|----------|--------------|--------|
| Linux | x64 | Tested |
| macOS | x64 | Supported |
| macOS | ARM64 (Apple Silicon) | Supported |
| Windows | x64 | Supported |

## Building for NuGet Distribution

```bash
# Build native library for your platform first
cd bindings/c
cargo build --release --no-default-features

# Copy to runtimes folder
cp ../../target/release/libzen_ffi.so ../dotnet/runtimes/linux-x64/native/

# Create NuGet package
cd ../dotnet
dotnet pack -c Release
```

The resulting `.nupkg` will include the native library for the current platform.

## Differences from Other Bindings

| Feature | Node.js | Python | C# (this) |
|---------|---------|--------|-----------|
| Technology | NAPI-RS | PyO3 | P/Invoke |
| Async Support | Native Promises | asyncio | Sync only* |
| Type Generation | TypeScript | .pyi stubs | Manual |
| Memory Management | Automatic | Automatic | Automatic |

*Async support can be added by wrapping calls in `Task.Run()`.

## Contributing

1. Fork the repository
2. Make changes to the C# bindings in `bindings/dotnet/`
3. Run tests: `dotnet test`
4. Submit a pull request

## License

MIT License - see [LICENSE](../../LICENSE) for details.
