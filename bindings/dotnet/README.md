# GoRules.Zen - C# Bindings for Zen Rules Engine

C# P/Invoke bindings for the Zen Rules Engine, providing high-performance business rules evaluation.

## Building

### Step 1: Build the Native Library

First, build the Rust C bindings as a shared library:

```bash
# From the repository root
cd bindings/c

# Build for your platform
cargo build --release

# The library will be at:
# - Linux:   target/release/libzen_ffi.so
# - macOS:   target/release/libzen_ffi.dylib
# - Windows: target/release/zen_ffi.dll
```

**Note:** The C bindings produce a static library by default. You need to change `Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]  # Change from "staticlib" to "cdylib"
```

### Step 2: Copy Native Library

Copy the built library to the appropriate runtime folder:

```bash
# Linux
mkdir -p bindings/dotnet/runtimes/linux-x64/native
cp target/release/libzen_ffi.so bindings/dotnet/runtimes/linux-x64/native/

# macOS x64
mkdir -p bindings/dotnet/runtimes/osx-x64/native
cp target/release/libzen_ffi.dylib bindings/dotnet/runtimes/osx-x64/native/

# macOS ARM64
mkdir -p bindings/dotnet/runtimes/osx-arm64/native
cp target/release/libzen_ffi.dylib bindings/dotnet/runtimes/osx-arm64/native/

# Windows
mkdir -p bindings/dotnet/runtimes/win-x64/native
cp target/release/zen_ffi.dll bindings/dotnet/runtimes/win-x64/native/
```

### Step 3: Build the C# Library

```bash
cd bindings/dotnet
dotnet build
```

## Usage

### Basic Expression Evaluation

```csharp
using GoRules.Zen;

// Evaluate a simple expression
var result = ZenExpression.Evaluate("a + b", """{"a": 10, "b": 20}""");
Console.WriteLine(result); // 30

// Evaluate a boolean expression
var isValid = ZenExpression.EvaluateUnary("age >= 18", """{"age": 21}""");
Console.WriteLine(isValid); // true

// Render a template
var greeting = ZenExpression.RenderTemplate(
    "Hello {{ name }}!",
    """{"name": "World"}"""
);
Console.WriteLine(greeting); // "Hello World!"
```

### Typed Expression Evaluation

```csharp
using GoRules.Zen;

var context = new { a = 10, b = 20 };
var result = ZenExpression.Evaluate<object, int>("a + b", context);
Console.WriteLine(result); // 30
```

### Decision Evaluation

```csharp
using GoRules.Zen;

// Create engine
using var engine = new ZenEngine();

// Load decision from JSON
string decisionJson = File.ReadAllText("my-decision.json");
using var decision = engine.CreateDecision(decisionJson);

// Evaluate
var result = decision.Evaluate("""{"input": "value"}""");
Console.WriteLine(result);

// With options
var options = new EvaluationOptions { Trace = true, MaxDepth = 10 };
var tracedResult = decision.Evaluate("""{"input": "value"}""", options);
```

### Using a Decision Loader

```csharp
using GoRules.Zen;

// Create engine with loader callback
using var engine = new ZenEngine(
    loader: key =>
    {
        // Load decision JSON by key from database, file, etc.
        var path = $"decisions/{key}.json";
        if (File.Exists(path))
            return File.ReadAllText(path);
        return null; // Not found
    }
);

// Evaluate by key - loader will be called
var result = engine.Evaluate("my-decision", """{"input": "value"}""");
```

### Custom Node Handler

```csharp
using GoRules.Zen;
using System.Text.Json;

using var engine = new ZenEngine(
    loader: key => File.ReadAllText($"decisions/{key}.json"),
    customNode: request =>
    {
        // Parse the request
        var doc = JsonDocument.Parse(request);
        var nodeType = doc.RootElement.GetProperty("node")
                         .GetProperty("kind").GetString();

        // Handle custom node types
        if (nodeType == "myCustomNode")
        {
            return JsonSerializer.Serialize(new
            {
                result = new { customOutput = "processed" }
            });
        }

        throw new Exception($"Unknown custom node: {nodeType}");
    }
);

var result = engine.Evaluate("decision-with-custom-node", "{}");
```

### Error Handling

```csharp
using GoRules.Zen;

try
{
    var result = ZenExpression.Evaluate("invalid expression !!!", "{}");
}
catch (ZenException ex)
{
    Console.WriteLine($"Error: {ex.ErrorCode}");
    Console.WriteLine($"Details: {ex.Details}");
}
```

## API Reference

### ZenExpression (Static Methods)

| Method | Description |
|--------|-------------|
| `Evaluate(expression, context)` | Evaluate an expression, returns JSON |
| `EvaluateUnary(expression, context)` | Evaluate a boolean expression |
| `RenderTemplate(template, context)` | Render a template string |

### ZenEngine

| Method | Description |
|--------|-------------|
| `ZenEngine()` | Create engine without callbacks |
| `ZenEngine(loader, customNode)` | Create engine with callbacks |
| `CreateDecision(json)` | Create decision from JSON |
| `GetDecision(key)` | Get decision via loader |
| `Evaluate(key, context, options)` | Evaluate decision by key |

### ZenDecision

| Method | Description |
|--------|-------------|
| `Evaluate(context, options)` | Evaluate the decision |
| `Dispose()` | Free native resources |

### EvaluationOptions

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `Trace` | bool | false | Enable execution trace |
| `MaxDepth` | byte | 0 | Max recursion depth (0 = default) |

## Platform Support

| Platform | Architecture | Status |
|----------|--------------|--------|
| Linux | x64 | ✅ |
| macOS | x64 | ✅ |
| macOS | ARM64 | ✅ |
| Windows | x64 | ✅ |

## License

MIT
