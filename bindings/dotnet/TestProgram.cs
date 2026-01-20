using System;
using GoRules.Zen;

class Program
{
    static void Main()
    {
        try
        {
            Console.WriteLine("Testing zen_ffi.dll loading...");
            
            // 尝试简单的表达式求值
            var result = ZenExpression.Evaluate("1 + 1", "{}");
            Console.WriteLine($"Result: {result}");
        }
        catch (Exception ex)
        {
            Console.WriteLine($"Error: {ex.Message}");
            Console.WriteLine($"Stack: {ex.StackTrace}");
        }
    }
}
