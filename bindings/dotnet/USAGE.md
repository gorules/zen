# Using GoRules.Zen in Your .NET Project

This guide explains how to integrate the Zen Rules Engine into your own .NET application.

## Installation Options

### Option 1: Reference the Project Directly

If you have the Zen repository cloned locally:

```bash
# Add project reference
dotnet add reference /path/to/zen/bindings/dotnet/GoRules.Zen.csproj
```

### Option 2: Build and Reference the DLL

```bash
# Build the library
cd /path/to/zen/bindings/dotnet
dotnet build -c Release

# Copy the DLL to your project
cp bin/Release/net10.0/GoRules.Zen.dll /path/to/your/project/

# Add reference in your .csproj
```

```xml
<ItemGroup>
  <Reference Include="GoRules.Zen">
    <HintPath>GoRules.Zen.dll</HintPath>
  </Reference>
</ItemGroup>
```

### Option 3: NuGet Package (Future)

```bash
# When published to NuGet
dotnet add package GoRules.Zen
```

## Native Library Setup

The C# bindings require the native Rust library. You must include it with your application.

### Building the Native Library

```bash
# From the zen repository root
cd bindings/c

# Build for your platform (without Go feature)
cargo build --release --no-default-features
```

### Output Locations

| Platform | Library Path |
|----------|--------------|
| Linux | `target/release/libzen_ffi.so` |
| macOS | `target/release/libzen_ffi.dylib` |
| Windows | `target/release/zen_ffi.dll` |

### Deploying the Native Library

The library includes a custom native library resolver that automatically searches for the native library in multiple locations. Use the recommended `runtimes/{rid}/native/` folder structure for cross-platform compatibility.

#### Recommended: Runtime Folders Structure

Create platform-specific runtime folders in your project:

```
YourProject/
├── YourProject.csproj
├── Program.cs
└── runtimes/
    ├── linux-x64/
    │   └── native/
    │       └── libzen_ffi.so
    ├── linux-arm64/
    │   └── native/
    │       └── libzen_ffi.so
    ├── osx-x64/
    │   └── native/
    │       └── libzen_ffi.dylib
    ├── osx-arm64/
    │   └── native/
    │       └── libzen_ffi.dylib
    ├── win-x64/
    │   └── native/
    │       └── zen_ffi.dll
    └── win-arm64/
        └── native/
            └── zen_ffi.dll
```

Add to your `.csproj`:

```xml
<ItemGroup>
  <!-- Linux x64 -->
  <None Include="runtimes/linux-x64/native/libzen_ffi.so"
        CopyToOutputDirectory="PreserveNewest"
        Condition="Exists('runtimes/linux-x64/native/libzen_ffi.so')" />

  <!-- Linux ARM64 -->
  <None Include="runtimes/linux-arm64/native/libzen_ffi.so"
        CopyToOutputDirectory="PreserveNewest"
        Condition="Exists('runtimes/linux-arm64/native/libzen_ffi.so')" />

  <!-- macOS x64 -->
  <None Include="runtimes/osx-x64/native/libzen_ffi.dylib"
        CopyToOutputDirectory="PreserveNewest"
        Condition="Exists('runtimes/osx-x64/native/libzen_ffi.dylib')" />

  <!-- macOS ARM64 -->
  <None Include="runtimes/osx-arm64/native/libzen_ffi.dylib"
        CopyToOutputDirectory="PreserveNewest"
        Condition="Exists('runtimes/osx-arm64/native/libzen_ffi.dylib')" />

  <!-- Windows x64 -->
  <None Include="runtimes/win-x64/native/zen_ffi.dll"
        CopyToOutputDirectory="PreserveNewest"
        Condition="Exists('runtimes/win-x64/native/zen_ffi.dll')" />

  <!-- Windows ARM64 -->
  <None Include="runtimes/win-arm64/native/zen_ffi.dll"
        CopyToOutputDirectory="PreserveNewest"
        Condition="Exists('runtimes/win-arm64/native/zen_ffi.dll')" />
</ItemGroup>
```

#### Library Search Order

The custom resolver searches for the native library in the following order:

1. `{AssemblyDir}/runtimes/{rid}/native/{libname}`
2. `{BaseDir}/runtimes/{rid}/native/{libname}`
3. `{AssemblyDir}/{libname}` (fallback)
4. `{BaseDir}/{libname}` (fallback)
5. Paths in `LD_LIBRARY_PATH` (Linux) or `DYLD_LIBRARY_PATH` (macOS)

#### Alternative: Environment Variables

You can also use environment variables to specify library search paths:

```bash
# Linux
export LD_LIBRARY_PATH=/path/to/native/libs:$LD_LIBRARY_PATH
dotnet run

# macOS
export DYLD_LIBRARY_PATH=/path/to/native/libs:$DYLD_LIBRARY_PATH
dotnet run
```

## Quick Start Example

### 1. Create a New Project

```bash
dotnet new console -n ZenDemo
cd ZenDemo
```

### 2. Add Reference and Native Library

```bash
# Add project reference
dotnet add reference /path/to/zen/bindings/dotnet/GoRules.Zen.csproj

# Copy native library
mkdir -p runtimes/linux-x64/native
cp /path/to/zen/target/release/libzen_ffi.so runtimes/linux-x64/native/
```

### 3. Write Your Code

```csharp
// Program.cs
using GoRules.Zen;

// Simple expression evaluation
var sum = ZenExpression.Evaluate("price * quantity", """
{
    "price": 29.99,
    "quantity": 3
}
""");
Console.WriteLine($"Total: {sum}");  // Total: 89.97

// Boolean check
var isEligible = ZenExpression.EvaluateUnary(">= 18", """{"$": 21}""");
Console.WriteLine($"Is eligible: {isEligible}");  // Is eligible: True

// Template rendering
var message = ZenExpression.RenderTemplate(
    "Order #{{ orderId }} confirmed for {{ customer.name }}",
    """
    {
        "orderId": 12345,
        "customer": { "name": "Alice" }
    }
    """
);
Console.WriteLine(message);  // "Order #12345 confirmed for Alice"
```

### 4. Run

```bash
dotnet run
```

## Common Use Cases

### Pricing Rules Engine

```csharp
using GoRules.Zen;

public class PricingService
{
    private readonly ZenEngine _engine;

    public PricingService()
    {
        _engine = new ZenEngine(
            loader: key => LoadDecisionFromDatabase(key)
        );
    }

    public decimal CalculateDiscount(string customerId, decimal orderTotal)
    {
        var context = new
        {
            customerId,
            orderTotal,
            isNewCustomer = CheckIfNewCustomer(customerId),
            loyaltyTier = GetLoyaltyTier(customerId)
        };

        var result = _engine.Evaluate<object, DiscountResult>(
            "discount-rules",
            context
        );

        return result.DiscountPercent;
    }

    private string? LoadDecisionFromDatabase(string key)
    {
        // Load from your database, file system, or API
        return File.Exists($"rules/{key}.json")
            ? File.ReadAllText($"rules/{key}.json")
            : null;
    }
}

public record DiscountResult(decimal DiscountPercent, string Reason);
```

### Form Validation

```csharp
using GoRules.Zen;

public class ValidationService
{
    public ValidationResult ValidateForm(FormData form)
    {
        var errors = new List<string>();

        // Email validation
        if (!ZenExpression.EvaluateUnary(
            """matches "^[\\w.-]+@[\\w.-]+\\.\\w+$" """,
            $$$"""{"$": "{{{form.Email}}}"}"""))
        {
            errors.Add("Invalid email format");
        }

        // Age validation
        if (!ZenExpression.EvaluateUnary(
            ">= 18",
            $$$"""{"$": {{{form.Age}}}}"""))
        {
            errors.Add("Must be 18 or older");
        }

        // Password strength
        if (!ZenExpression.EvaluateUnary(
            ">= 8",
            $$$"""{"$": {{{form.Password.Length}}}}"""))
        {
            errors.Add("Password must be at least 8 characters");
        }

        return new ValidationResult(errors.Count == 0, errors);
    }
}
```

### Dynamic Feature Flags

```csharp
using GoRules.Zen;

public class FeatureFlagService
{
    public bool IsFeatureEnabled(string featureName, UserContext user)
    {
        var context = new
        {
            feature = featureName,
            user = new
            {
                id = user.Id,
                tier = user.Tier,
                region = user.Region,
                registrationDate = user.RegisteredAt.ToString("o")
            },
            environment = Environment.GetEnvironmentVariable("ASPNETCORE_ENVIRONMENT")
        };

        try
        {
            using var engine = new ZenEngine(
                loader: _ => File.ReadAllText("feature-flags.json")
            );

            var result = engine.Evaluate<object, FeatureFlagResult>(
                "feature-flags",
                context
            );

            return result.Enabled;
        }
        catch (ZenException)
        {
            // Default to disabled on error
            return false;
        }
    }
}
```

### ASP.NET Core Integration

```csharp
// Program.cs
using GoRules.Zen;

var builder = WebApplication.CreateBuilder(args);

// Register as singleton (thread-safe)
builder.Services.AddSingleton<ZenEngine>(sp =>
{
    return new ZenEngine(
        loader: key =>
        {
            var path = Path.Combine("Rules", $"{key}.json");
            return File.Exists(path) ? File.ReadAllText(path) : null;
        }
    );
});

var app = builder.Build();

app.MapPost("/api/evaluate/{ruleKey}", async (
    string ruleKey,
    JsonElement context,
    ZenEngine engine) =>
{
    try
    {
        var result = engine.Evaluate(ruleKey, context.GetRawText());
        return Results.Ok(JsonDocument.Parse(result).RootElement);
    }
    catch (ZenException ex)
    {
        return Results.BadRequest(new { error = ex.Message });
    }
});

app.Run();
```

### Batch Processing

```csharp
using GoRules.Zen;
using System.Collections.Concurrent;

public class BatchProcessor
{
    public async Task<List<ProcessingResult>> ProcessBatchAsync(
        IEnumerable<Order> orders)
    {
        var results = new ConcurrentBag<ProcessingResult>();

        // ZenEngine is thread-safe for evaluation
        using var engine = new ZenEngine(
            loader: key => File.ReadAllText($"rules/{key}.json")
        );

        await Parallel.ForEachAsync(orders, async (order, ct) =>
        {
            await Task.Run(() =>
            {
                var context = JsonSerializer.Serialize(order);
                var result = engine.Evaluate("order-processing", context);
                results.Add(new ProcessingResult(order.Id, result));
            }, ct);
        });

        return results.ToList();
    }
}
```

## Decision JSON Format

Zen uses a JSON format for decision graphs. Here's a minimal example:

```json
{
  "contentType": "application/vnd.gorules.decision",
  "nodes": [
    {
      "id": "input",
      "type": "inputNode",
      "position": { "x": 0, "y": 0 },
      "name": "Request"
    },
    {
      "id": "table1",
      "type": "decisionTableNode",
      "position": { "x": 200, "y": 0 },
      "name": "Pricing Table",
      "content": {
        "rules": [
          {
            "conditions": [{ "field": "tier", "operator": "==", "value": "gold" }],
            "outputs": [{ "field": "discount", "value": "0.20" }]
          },
          {
            "conditions": [{ "field": "tier", "operator": "==", "value": "silver" }],
            "outputs": [{ "field": "discount", "value": "0.10" }]
          }
        ]
      }
    },
    {
      "id": "output",
      "type": "outputNode",
      "position": { "x": 400, "y": 0 },
      "name": "Response"
    }
  ],
  "edges": [
    { "id": "e1", "sourceId": "input", "targetId": "table1" },
    { "id": "e2", "sourceId": "table1", "targetId": "output" }
  ]
}
```

For a visual editor and more complex examples, visit [GoRules Editor](https://editor.gorules.io/).

## Error Handling Best Practices

```csharp
using GoRules.Zen;
using GoRules.Zen.Interop;

public class SafeRulesEvaluator
{
    private readonly ZenEngine _engine;
    private readonly ILogger<SafeRulesEvaluator> _logger;

    public SafeRulesEvaluator(ILogger<SafeRulesEvaluator> logger)
    {
        _logger = logger;
        _engine = new ZenEngine(loader: LoadRule);
    }

    public EvaluationResult Evaluate(string ruleKey, object context)
    {
        try
        {
            var contextJson = JsonSerializer.Serialize(context);
            var result = _engine.Evaluate(ruleKey, contextJson);

            return new EvaluationResult
            {
                Success = true,
                Data = JsonSerializer.Deserialize<JsonElement>(result)
            };
        }
        catch (ZenException ex) when (ex.ErrorCode == ZenErrorCode.LoaderKeyNotFound)
        {
            _logger.LogWarning("Rule not found: {RuleKey}", ruleKey);
            return new EvaluationResult
            {
                Success = false,
                Error = $"Rule '{ruleKey}' not found"
            };
        }
        catch (ZenException ex) when (ex.ErrorCode == ZenErrorCode.EvaluationError)
        {
            _logger.LogError(ex, "Evaluation failed for rule: {RuleKey}", ruleKey);
            return new EvaluationResult
            {
                Success = false,
                Error = "Rule evaluation failed",
                Details = ex.Details
            };
        }
        catch (ZenException ex)
        {
            _logger.LogError(ex, "Unexpected Zen error: {ErrorCode}", ex.ErrorCode);
            return new EvaluationResult
            {
                Success = false,
                Error = ex.Message
            };
        }
    }

    private string? LoadRule(string key)
    {
        // Implement your rule loading logic
        return null;
    }
}

public class EvaluationResult
{
    public bool Success { get; init; }
    public JsonElement? Data { get; init; }
    public string? Error { get; init; }
    public string? Details { get; init; }
}
```

## Performance Tips

1. **Reuse ZenEngine instances** - Creating an engine is relatively expensive. Create once and reuse.

2. **Use decision caching** - The loader is called for each evaluation. Implement caching:

```csharp
public class CachingDecisionLoader
{
    private readonly ConcurrentDictionary<string, string> _cache = new();

    public string? Load(string key)
    {
        return _cache.GetOrAdd(key, k =>
        {
            var path = $"rules/{k}.json";
            return File.Exists(path) ? File.ReadAllText(path) : null!;
        });
    }

    public void InvalidateCache(string? key = null)
    {
        if (key != null)
            _cache.TryRemove(key, out _);
        else
            _cache.Clear();
    }
}
```

3. **Compile decisions once** - For frequently-used decisions, use `CreateDecision` once and reuse:

```csharp
// Good - compile once
using var decision = engine.CreateDecision(decisionJson);
foreach (var item in items)
{
    decision.Evaluate(JsonSerializer.Serialize(item));
}

// Bad - recompiles every time
foreach (var item in items)
{
    engine.Evaluate("my-rule", JsonSerializer.Serialize(item));
}
```

4. **Use appropriate MaxDepth** - Lower values are faster but limit recursion depth.

## Troubleshooting

### DllNotFoundException

**Error:** `Unable to load shared library 'zen_ffi'`

**Solutions:**
1. Ensure the native library is in the same directory as your executable
2. Check you built with `--no-default-features` flag
3. Verify the library architecture matches your runtime (x64 vs ARM64)

### DepthLimitExceeded

**Error:** `Evaluation error: {"type":"DepthLimitExceeded"}`

**Solution:** Increase `MaxDepth` in evaluation options:

```csharp
var options = new EvaluationOptions { MaxDepth = 10 };
decision.Evaluate(context, options);
```

### Undefined Symbol Errors

**Error:** `undefined symbol: zen_engine_go_custom_node_callback`

**Solution:** Rebuild the native library without the Go feature:

```bash
cargo build --release --no-default-features
```

## Next Steps

- Read the [README.md](README.md) for API reference
- Explore the [test files](GoRules.Zen.Tests/) for more examples
- Visit [GoRules documentation](https://docs.gorules.io/) for decision modeling
- Try the [visual editor](https://editor.gorules.io/) to create decisions
