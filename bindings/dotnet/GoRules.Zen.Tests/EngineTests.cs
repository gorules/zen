using System;
using Xunit;
using GoRules.Zen;
using GoRules.Zen.Interop;

namespace GoRules.Zen.Tests;

public class EngineTests : IDisposable
{
    private ZenEngine? _engine;

    public void Dispose()
    {
        _engine?.Dispose();
    }

    [Fact]
    public void Constructor_Default_CreatesEngine()
    {
        _engine = new ZenEngine();
        Assert.NotNull(_engine);
    }

    [Fact]
    public void Constructor_WithLoader_CreatesEngine()
    {
        _engine = new ZenEngine(
            loader: key => null
        );
        Assert.NotNull(_engine);
    }

    [Fact]
    public void Constructor_WithLoaderAndCustomNode_CreatesEngine()
    {
        _engine = new ZenEngine(
            loader: key => null,
            customNode: request => """{"result": {}}"""
        );
        Assert.NotNull(_engine);
    }

    [Fact]
    public void CreateDecision_ValidJson_ReturnsDecision()
    {
        _engine = new ZenEngine();

        // Minimal valid decision JSON
        var decisionJson = """
        {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {
                    "id": "input",
                    "type": "inputNode",
                    "position": {"x": 0, "y": 0},
                    "name": "Input"
                },
                {
                    "id": "output",
                    "type": "outputNode",
                    "position": {"x": 200, "y": 0},
                    "name": "Output"
                }
            ],
            "edges": [
                {
                    "id": "edge1",
                    "sourceId": "input",
                    "targetId": "output"
                }
            ]
        }
        """;

        using var decision = _engine.CreateDecision(decisionJson);
        Assert.NotNull(decision);
    }

    [Fact]
    public void CreateDecision_InvalidJson_ThrowsZenException()
    {
        _engine = new ZenEngine();

        var ex = Assert.Throws<ZenException>(() =>
            _engine.CreateDecision("not valid json")
        );
        Assert.True(
            ex.ErrorCode == ZenErrorCode.JsonDeserializationFailed ||
            ex.ErrorCode == ZenErrorCode.InvalidArgument
        );
    }

    [Fact]
    public void Decision_Evaluate_ReturnsResult()
    {
        _engine = new ZenEngine();

        var decisionJson = """
        {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {
                    "id": "input",
                    "type": "inputNode",
                    "position": {"x": 0, "y": 0},
                    "name": "Input"
                },
                {
                    "id": "output",
                    "type": "outputNode",
                    "position": {"x": 200, "y": 0},
                    "name": "Output"
                }
            ],
            "edges": [
                {
                    "id": "edge1",
                    "sourceId": "input",
                    "targetId": "output"
                }
            ]
        }
        """;

        using var decision = _engine.CreateDecision(decisionJson);
        var result = decision.Evaluate("""{"test": "value"}""");

        Assert.NotNull(result);
        Assert.Contains("result", result);
    }

    [Fact]
    public void Decision_EvaluateWithTrace_IncludesTrace()
    {
        _engine = new ZenEngine();

        var decisionJson = """
        {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {
                    "id": "input",
                    "type": "inputNode",
                    "position": {"x": 0, "y": 0},
                    "name": "Input"
                },
                {
                    "id": "output",
                    "type": "outputNode",
                    "position": {"x": 200, "y": 0},
                    "name": "Output"
                }
            ],
            "edges": [
                {
                    "id": "edge1",
                    "sourceId": "input",
                    "targetId": "output"
                }
            ]
        }
        """;

        using var decision = _engine.CreateDecision(decisionJson);
        var options = new EvaluationOptions { Trace = true, MaxDepth = 5 };
        var result = decision.Evaluate("""{"test": "value"}""", options);

        Assert.NotNull(result);
        Assert.Contains("trace", result);
    }

    [Fact]
    public void GetDecision_WithLoader_CallsLoader()
    {
        var loaderCalled = false;
        var requestedKey = "";

        var decisionJson = """
        {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {
                    "id": "input",
                    "type": "inputNode",
                    "position": {"x": 0, "y": 0},
                    "name": "Input"
                },
                {
                    "id": "output",
                    "type": "outputNode",
                    "position": {"x": 200, "y": 0},
                    "name": "Output"
                }
            ],
            "edges": [
                {
                    "id": "edge1",
                    "sourceId": "input",
                    "targetId": "output"
                }
            ]
        }
        """;

        _engine = new ZenEngine(
            loader: key =>
            {
                loaderCalled = true;
                requestedKey = key;
                return decisionJson;
            }
        );

        using var decision = _engine.GetDecision("my-decision");

        Assert.True(loaderCalled);
        Assert.Equal("my-decision", requestedKey);
    }

    [Fact]
    public void GetDecision_LoaderReturnsNull_ThrowsZenException()
    {
        _engine = new ZenEngine(
            loader: key => null
        );

        var ex = Assert.Throws<ZenException>(() =>
            _engine.GetDecision("missing-decision")
        );

        Assert.True(
            ex.ErrorCode == ZenErrorCode.LoaderKeyNotFound ||
            ex.ErrorCode == ZenErrorCode.LoaderInternalError
        );
    }

    [Fact]
    public void Evaluate_ByKey_UsesLoaderAndReturnsResult()
    {
        var decisionJson = """
        {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {
                    "id": "input",
                    "type": "inputNode",
                    "position": {"x": 0, "y": 0},
                    "name": "Input"
                },
                {
                    "id": "output",
                    "type": "outputNode",
                    "position": {"x": 200, "y": 0},
                    "name": "Output"
                }
            ],
            "edges": [
                {
                    "id": "edge1",
                    "sourceId": "input",
                    "targetId": "output"
                }
            ]
        }
        """;

        _engine = new ZenEngine(
            loader: key => key == "test-decision" ? decisionJson : null
        );

        var result = _engine.Evaluate("test-decision", """{"input": "data"}""");

        Assert.NotNull(result);
        Assert.Contains("result", result);
    }

    [Fact]
    public void Dispose_CalledMultipleTimes_DoesNotThrow()
    {
        _engine = new ZenEngine();
        _engine.Dispose();
        _engine.Dispose(); // Should not throw
    }

    [Fact]
    public void Evaluate_AfterDispose_ThrowsObjectDisposedException()
    {
        _engine = new ZenEngine();
        _engine.Dispose();

        Assert.Throws<ObjectDisposedException>(() =>
            _engine.CreateDecision("{}")
        );
    }
}
