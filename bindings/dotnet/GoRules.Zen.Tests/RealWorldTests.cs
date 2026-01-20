using System;
using System.IO;
using System.Text.Json;
using Xunit;
using GoRules.Zen;

namespace GoRules.Zen.Tests;

public class RealWorldTests : IDisposable
{
    private ZenEngine? _engine;

    public void Dispose()
    {
        _engine?.Dispose();
    }

    private static string GetDecisionsPath()
    {
        // Navigate from bin/Debug/net10.0 up to the decisions folder
        var testDir = AppContext.BaseDirectory;
        var decisionsPath = Path.GetFullPath(Path.Combine(testDir, "..", "..", "..", "..", "decisions"));
        return decisionsPath;
    }

    [Fact]
    public void CompanyAnalysisTest()
    {
        _engine = new ZenEngine();

        var decisionPath = Path.Combine(GetDecisionsPath(), "1.company-analysis.json");
        var decisionJson = File.ReadAllText(decisionPath);

        using var decision = _engine.CreateDecision(decisionJson);

        var context = """
        {
            "country": "US",
            "dateInc": "2014-12-31T16:00:00.000Z",
            "industryType": "HC",
            "annualRevenue": 1500000,
            "creditRating": 770,
            "companySize": "medium"
        }
        """;

        var resultJson = decision.Evaluate(context);
        using var doc = JsonDocument.Parse(resultJson);
        var root = doc.RootElement;

        Assert.True(root.TryGetProperty("result", out var result), "Result should contain 'result' property");

        // Verify flag values
        Assert.True(result.TryGetProperty("flag", out var flag), "Result should contain 'flag' property");
        Assert.Equal("amber", flag.GetProperty("annualRevenue").GetString());
        Assert.Equal("amber", flag.GetProperty("companySize").GetString());
        Assert.Equal("green", flag.GetProperty("country").GetString());
        Assert.Equal("green", flag.GetProperty("creditRating").GetString());
        Assert.Equal("green", flag.GetProperty("industryType").GetString());
        Assert.Equal("green", flag.GetProperty("years").GetString());

        // Verify comment values
        Assert.True(result.TryGetProperty("comment", out var comment), "Result should contain 'comment' property");
        Assert.Equal("Medium - Established market presence", comment.GetProperty("annualRevenue").GetString());
        Assert.Equal("Moderate risk with more stability", comment.GetProperty("companySize").GetString());
        Assert.Equal("Very Good - Low risk", comment.GetProperty("creditRating").GetString());
        Assert.Equal("Essential services with constant demand", comment.GetProperty("industryType").GetString());
        Assert.Equal("Mature business, proven market resilience and operational stability", comment.GetProperty("years").GetString());
    }

    [Fact]
    public void DynamicPricingTest()
    {
        _engine = new ZenEngine();

        var decisionPath = Path.Combine(GetDecisionsPath(), "2.dynamic-pricing.json");
        var decisionJson = File.ReadAllText(decisionPath);

        using var decision = _engine.CreateDecision(decisionJson);

        var context = """
        {
            "pricing": {
                "basePrice": 100,
                "demand": "high",
                "timeOfDay": "normal",
                "competitorPrice": "equal",
                "customerSegment": "regular"
            }
        }
        """;

        var resultJson = decision.Evaluate(context);
        using var doc = JsonDocument.Parse(resultJson);
        var root = doc.RootElement;

        Assert.True(root.TryGetProperty("result", out var result), "Result should contain 'result' property");

        // Verify adjusted price
        Assert.Equal(115, result.GetProperty("adjustedPrice").GetDouble());

        // Verify final adjustment factor
        Assert.Equal(1.15, result.GetProperty("finalAdjustmentFactor").GetDouble());

        // Verify adjustments
        Assert.True(result.TryGetProperty("adjustments", out var adjustments), "Result should contain 'adjustments' property");
        Assert.Equal(1, adjustments.GetProperty("competitor").GetDouble());
        Assert.Equal(1.15, adjustments.GetProperty("demand").GetDouble());
        Assert.Equal(1, adjustments.GetProperty("segment").GetDouble());
        Assert.Equal(1, adjustments.GetProperty("time").GetDouble());

        // Verify pricing passthrough
        Assert.True(result.TryGetProperty("pricing", out var pricing), "Result should contain 'pricing' property");
        Assert.Equal(100, pricing.GetProperty("basePrice").GetInt32());
        Assert.Equal("equal", pricing.GetProperty("competitorPrice").GetString());
        Assert.Equal("regular", pricing.GetProperty("customerSegment").GetString());
        Assert.Equal("high", pricing.GetProperty("demand").GetString());
        Assert.Equal("normal", pricing.GetProperty("timeOfDay").GetString());
    }

    [Fact]
    public void RealTimeQuotationTest()
    {
        _engine = new ZenEngine();

        var decisionPath = Path.Combine(GetDecisionsPath(), "3.real-time-quotation.json");
        var decisionJson = File.ReadAllText(decisionPath);

        using var decision = _engine.CreateDecision(decisionJson);

        var context = """
        {
            "generalLiability": 5000000,
            "commercialProperty": 1000000,
            "professionalIndemnity": 1000000
        }
        """;

        var resultJson = decision.Evaluate(context);
        using var doc = JsonDocument.Parse(resultJson);
        var root = doc.RootElement;

        Assert.True(root.TryGetProperty("result", out var result), "Result should contain 'result' property");

        // Verify quotation values
        Assert.Equal("300.3", result.GetProperty("paymentFee").GetString());
        Assert.Equal("7800", result.GetProperty("premium").GetString());
        Assert.Equal("780", result.GetProperty("tax").GetString());
        Assert.Equal("8880.3", result.GetProperty("total").GetString());
    }
}
