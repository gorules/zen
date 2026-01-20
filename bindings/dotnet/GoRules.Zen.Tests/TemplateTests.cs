using Xunit;
using GoRules.Zen;

namespace GoRules.Zen.Tests;

public class TemplateTests
{
    [Fact]
    public void RenderTemplate_SimpleInterpolation_ReturnsCorrectResult()
    {
        var result = ZenExpression.RenderTemplate(
            "Hello {{ name }}!",
            """{"name": "World"}"""
        );
        Assert.Equal("\"Hello World!\"", result);
    }

    [Fact]
    public void RenderTemplate_MultipleVariables_ReturnsCorrectResult()
    {
        var result = ZenExpression.RenderTemplate(
            "{{ greeting }}, {{ name }}!",
            """{"greeting": "Hi", "name": "Alice"}"""
        );
        Assert.Equal("\"Hi, Alice!\"", result);
    }

    [Fact]
    public void RenderTemplate_NestedProperty_ReturnsCorrectResult()
    {
        var result = ZenExpression.RenderTemplate(
            "User: {{ user.name }} ({{ user.email }})",
            """{"user": {"name": "Bob", "email": "bob@example.com"}}"""
        );
        Assert.Equal("\"User: Bob (bob@example.com)\"", result);
    }

    [Fact]
    public void RenderTemplate_WithExpression_ReturnsCorrectResult()
    {
        var result = ZenExpression.RenderTemplate(
            "Total: {{ price * quantity }}",
            """{"price": 10, "quantity": 3}"""
        );
        Assert.Equal("\"Total: 30\"", result);
    }

    [Fact]
    public void RenderTemplate_TypedContext_ReturnsCorrectResult()
    {
        var context = new { productName = "Widget", price = 19.99 };
        var result = ZenExpression.RenderTemplate(
            "{{ productName }}: ${{ price }}",
            context
        );
        Assert.Contains("Widget", result);
        Assert.Contains("19.99", result);
    }
}
