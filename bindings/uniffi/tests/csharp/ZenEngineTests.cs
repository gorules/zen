using System.IO.Compression;
using System.Text.Json.Nodes;
using GoRules.ZenEngine;
using Xunit;

namespace GoRules.ZenEngine.Tests;

public class ZenEngineTests
{
    private static string TestDataRoot()
    {
        var current = new DirectoryInfo(AppContext.BaseDirectory);
        while (current != null && !Directory.Exists(Path.Combine(current.FullName, "test-data")))
        {
            current = current.Parent;
        }

        if (current == null)
        {
            throw new InvalidOperationException("test-data directory not found");
        }

        return Path.Combine(current.FullName, "test-data");
    }

    private static JsonBuffer ReadTestFile(string name) =>
        new(File.ReadAllBytes(Path.Combine(TestDataRoot(), name)));

    private static JsonNode Json(JsonBuffer buffer) =>
        JsonNode.Parse(buffer.ToString())!;

    private class FilesystemCallback : ZenDecisionLoaderCallback
    {
        public Task<JsonBuffer?> Load(string key)
        {
            var path = Path.Combine(TestDataRoot(), key);
            return Task.FromResult<JsonBuffer?>(File.Exists(path) ? new JsonBuffer(File.ReadAllBytes(path)) : null);
        }
    }

    private class SumNodeCallback : ZenCustomNodeCallback
    {
        public ZenEngineHandlerRequest? SeenRequest;

        public Task<ZenEngineHandlerResponse> Handle(ZenEngineHandlerRequest key)
        {
            SeenRequest = key;
            var a = Json(key.Input)["a"]!.GetValue<int>();
            var output = new JsonBuffer($"{{\"data\":{a + 20}}}");
            return Task.FromResult(new ZenEngineHandlerResponse(output, null));
        }
    }

    [Fact]
    public async Task StaticLoader()
    {
        var loader = new ZenLoader.Static(new Dictionary<string, JsonBuffer>
        {
            ["table.json"] = ReadTestFile("table.json"),
        });

        using var engine = new ZenEngine(loader);
        var response = await engine.Evaluate("table.json", new JsonBuffer("{\"input\":12}"), null);
        Assert.Equal(10, Json(response.Result)["output"]!.GetValue<int>());
        Assert.Null(response.Trace);
    }

    [Fact]
    public async Task FilesystemLoader()
    {
        using var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()));
        var response = await engine.Evaluate("table.json", new JsonBuffer("{\"input\":5}"), null);
        Assert.Equal(0, Json(response.Result)["output"]!.GetValue<int>());
    }

    [Fact]
    public async Task ZipLoader()
    {
        using var buffer = new MemoryStream();
        using (var zip = new ZipArchive(buffer, ZipArchiveMode.Create, true))
        {
            var entry = zip.CreateEntry("table.json");
            using var stream = entry.Open();
            stream.Write(ReadTestFile("table.json").Value);
        }

        using var engine = new ZenEngine(new ZenLoader.Zip(buffer.ToArray()));
        var response = await engine.Evaluate("table.json", new JsonBuffer("{\"input\":12}"), null);
        Assert.Equal(10, Json(response.Result)["output"]!.GetValue<int>());
    }

    [Fact]
    public void InvalidZipFailsOnConstruction()
    {
        Assert.ThrowsAny<ZenException>(() => new ZenEngine(new ZenLoader.Zip(new byte[] { 1, 2, 3, 4 })));
    }

    [Fact]
    public async Task CallbackLoader()
    {
        using var engine = new ZenEngine(new ZenLoader.Callback(new FilesystemCallback()));
        var response = await engine.Evaluate("table.json", new JsonBuffer("{\"input\":12}"), null);
        Assert.Equal(10, Json(response.Result)["output"]!.GetValue<int>());

        await Assert.ThrowsAnyAsync<ZenException>(() =>
            engine.Evaluate("missing.json", new JsonBuffer("{}"), null));
    }

    [Fact]
    public async Task MissingKeyFails()
    {
        using var engine = new ZenEngine(new ZenLoader.Static(new Dictionary<string, JsonBuffer>()));
        await Assert.ThrowsAnyAsync<ZenException>(() =>
            engine.Evaluate("missing.json", new JsonBuffer("{}"), null));
    }

    [Fact]
    public async Task CreateDecision()
    {
        using var engine = new ZenEngine();
        using var decision = engine.CreateDecision(ReadTestFile("table.json"));
        decision.Validate();
        var response = await decision.Evaluate(new JsonBuffer("{\"input\":12}"), null);
        Assert.Equal(10, Json(response.Result)["output"]!.GetValue<int>());
    }

    [Fact]
    public async Task GetDecision()
    {
        using var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()));
        using var decision = await engine.GetDecision("table.json");
        var response = await decision.Evaluate(new JsonBuffer("{\"input\":12}"), null);
        Assert.Equal(10, Json(response.Result)["output"]!.GetValue<int>());
    }

    [Fact]
    public async Task EvaluateBatch()
    {
        using var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()));
        var results = await engine.EvaluateBatch(new[]
        {
            new ZenBatchRequest("table.json", new JsonBuffer("{\"input\":12}")),
            new ZenBatchRequest("missing.json", new JsonBuffer("{}")),
            new ZenBatchRequest("table.json", new JsonBuffer("{\"input\":5}")),
        }, null);

        Assert.Equal(3, results.Length);
        Assert.True(results[0].Success);
        Assert.Equal(10, Json(results[0].Data!.Result)["output"]!.GetValue<int>());
        Assert.False(results[1].Success);
        Assert.NotNull(results[1].Error);
        Assert.True(results[2].Success);
        Assert.Equal(0, Json(results[2].Data!.Result)["output"]!.GetValue<int>());
    }

    [Fact]
    public async Task TraceOption()
    {
        using var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()));
        var options = new ZenEvaluateOptions(5, true);
        var response = await engine.Evaluate("table.json", new JsonBuffer("{\"input\":12}"), options);
        Assert.NotNull(response.Trace);
        Assert.NotEmpty(response.Trace);
    }

    [Fact]
    public async Task ExpressionGraph()
    {
        using var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()));
        var context = new JsonBuffer("{\"numbers\":[1,5,15,25],\"firstName\":\"John\",\"lastName\":\"Doe\"}");
        var response = await engine.Evaluate("expression.json", context, null);
        var expected = JsonNode.Parse(
            "{\"deep\":{\"nested\":{\"sum\":46}},\"fullName\":\"John Doe\",\"largeNumbers\":[15,25],\"smallNumbers\":[1,5]}");
        Assert.True(JsonNode.DeepEquals(expected, Json(response.Result)));
    }

    [Fact]
    public async Task FunctionGraph()
    {
        using var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()));
        var response = await engine.Evaluate("function.json", new JsonBuffer("{\"input\":15}"), null);
        Assert.Equal(30, Json(response.Result)["output"]!.GetValue<int>());
    }

    [Fact]
    public async Task CustomNodeHandler()
    {
        var handler = new SumNodeCallback();
        using (var engine = new ZenEngine(new ZenLoader.Filesystem(TestDataRoot()), handler))
        {
            var response = await engine.Evaluate("custom.json", new JsonBuffer("{\"a\":5}"), null);
            Assert.Equal(25, Json(response.Result)["data"]!.GetValue<int>());
        }

        Assert.NotNull(handler.SeenRequest);
        Assert.Equal("sum", handler.SeenRequest!.Node.Kind);
        Assert.Equal("customNode1", handler.SeenRequest.Node.Name);
        Assert.Equal("{{ a + 10 }}", Json(handler.SeenRequest.Node.Config)["prop1"]!.GetValue<string>());
    }

    [Fact]
    public void EvaluateExpression()
    {
        var result = ZenUniffiMethods.EvaluateExpression("sum(numbers)", new JsonBuffer("{\"numbers\":[1,2,3]}"));
        Assert.Equal(6, Json(result).GetValue<int>());
    }

    [Fact]
    public void EvaluateUnaryExpression()
    {
        Assert.True(ZenUniffiMethods.EvaluateUnaryExpression("$ > 10", new JsonBuffer("{\"$\":15}")));
        Assert.False(ZenUniffiMethods.EvaluateUnaryExpression("$ > 10", new JsonBuffer("{\"$\":5}")));
    }

    [Fact]
    public void InvalidExpressionFails()
    {
        Assert.ThrowsAny<ZenException>(() =>
            ZenUniffiMethods.EvaluateExpression("a +* b", new JsonBuffer("{}")));
    }

    [Fact]
    public void CompiledExpression()
    {
        using (var expression = ZenExpression.Compile("a + b"))
        {
            var result = expression.Evaluate(new JsonBuffer("{\"a\":1,\"b\":2}"));
            Assert.Equal(3, Json(result).GetValue<int>());
        }

        using var unary = ZenExpressionUnary.Compile("$ > 3");
        Assert.True(unary.Evaluate(new JsonBuffer("{\"$\":4}")));
        Assert.False(unary.Evaluate(new JsonBuffer("{\"$\":2}")));
    }
}
