using Xunit;
using GoRules.Zen;
using GoRules.Zen.Interop;

namespace GoRules.Zen.Tests;

public class ExpressionTests
{
    [Fact]
    public void Evaluate_SimpleAddition_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate("a + b", """{"a": 10, "b": 20}""");
        Assert.Equal("30", result);
    }

    [Fact]
    public void Evaluate_Multiplication_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate("a * b", """{"a": 5, "b": 4}""");
        Assert.Equal("20", result);
    }

    [Fact]
    public void Evaluate_StringConcat_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate(
            "firstName + \" \" + lastName",
            """{"firstName": "John", "lastName": "Doe"}"""
        );
        Assert.Equal("\"John Doe\"", result);
    }

    [Fact]
    public void Evaluate_ArrayAccess_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate(
            "items[1]",
            """{"items": [10, 20, 30]}"""
        );
        Assert.Equal("20", result);
    }

    [Fact]
    public void Evaluate_ObjectProperty_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate(
            "user.name",
            """{"user": {"name": "Alice", "age": 30}}"""
        );
        Assert.Equal("\"Alice\"", result);
    }

    [Fact]
    public void Evaluate_Ternary_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate(
            "age >= 18 ? \"adult\" : \"minor\"",
            """{"age": 21}"""
        );
        Assert.Equal("\"adult\"", result);
    }

    [Fact]
    public void Evaluate_Max_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate("max(items)", """{"items": [5, 10, 3]}""");
        Assert.Equal("10", result);
    }

    [Fact]
    public void Evaluate_Min_ReturnsCorrectResult()
    {
        var result = ZenExpression.Evaluate("min(items)", """{"items": [5, 10, 3]}""");
        Assert.Equal("3", result);
    }

    [Fact]
    public void Evaluate_TypedContext_ReturnsTypedResult()
    {
        var context = new { a = 15, b = 25 };
        var result = ZenExpression.Evaluate<object, int>("a + b", context);
        Assert.Equal(40, result);
    }

    [Fact]
    public void Evaluate_InvalidExpression_ThrowsZenException()
    {
        var ex = Assert.Throws<ZenException>(() =>
            ZenExpression.Evaluate("invalid syntax !!!", "{}")
        );
        Assert.True(ex.ErrorCode == ZenErrorCode.EvaluationError || ex.ErrorCode == ZenErrorCode.IsolateError);
    }

    [Fact]
    public void Evaluate_InvalidJson_ThrowsZenException()
    {
        var ex = Assert.Throws<ZenException>(() =>
            ZenExpression.Evaluate("a + b", "not valid json")
        );
        Assert.True(
            ex.ErrorCode == ZenErrorCode.JsonDeserializationFailed ||
            ex.ErrorCode == ZenErrorCode.EvaluationError ||
            ex.ErrorCode == ZenErrorCode.IsolateError
        );
    }
}
