using Xunit;
using GoRules.Zen;

namespace GoRules.Zen.Tests;

public class UnaryExpressionTests
{
    [Fact]
    public void EvaluateUnary_GreaterThan_ReturnsTrue()
    {
        var result = ZenExpression.EvaluateUnary("> 18", """{"$": 21}""");
        Assert.True(result);
    }

    [Fact]
    public void EvaluateUnary_GreaterThan_ReturnsFalse()
    {
        var result = ZenExpression.EvaluateUnary("> 18", """{"$": 16}""");
        Assert.False(result);
    }

    [Fact]
    public void EvaluateUnary_Equality_ReturnsTrue()
    {
        var result = ZenExpression.EvaluateUnary(
            "== \"active\"",
            """{"$": "active"}"""
        );
        Assert.True(result);
    }

    [Fact]
    public void EvaluateUnary_GreaterThanOrEqual_ReturnsTrue()
    {
        var result = ZenExpression.EvaluateUnary(
            ">= 18",
            """{"$": 25}"""
        );
        Assert.True(result);
    }

    [Fact]
    public void EvaluateUnary_LessThan_ReturnsTrue()
    {
        var result = ZenExpression.EvaluateUnary(
            "< 100",
            """{"$": 50}"""
        );
        Assert.True(result);
    }

    [Fact]
    public void EvaluateUnary_NotEqual_ReturnsTrue()
    {
        var result = ZenExpression.EvaluateUnary(
            "!= \"blocked\"",
            """{"$": "active"}"""
        );
        Assert.True(result);
    }

    [Fact]
    public void EvaluateUnary_InArray_ReturnsTrue()
    {
        var result = ZenExpression.EvaluateUnary(
            "in [\"active\", \"pending\"]",
            """{"$": "active"}"""
        );
        Assert.True(result);
    }

    [Fact]
    public void EvaluateUnary_NotInArray_ReturnsFalse()
    {
        var result = ZenExpression.EvaluateUnary(
            "in [\"active\", \"pending\"]",
            """{"$": "blocked"}"""
        );
        Assert.False(result);
    }
}
