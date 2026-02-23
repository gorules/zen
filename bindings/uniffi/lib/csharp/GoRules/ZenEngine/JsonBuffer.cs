namespace GoRules.ZenEngine;

using System.Text;

public class JsonBuffer
{
    public byte[] Value { get; }

    public JsonBuffer(byte[] value)
    {
        Value = value;
    }

    public JsonBuffer(string json)
    {
        Value = Encoding.UTF8.GetBytes(json);
    }

    public override string ToString()
    {
        return Encoding.UTF8.GetString(Value);
    }
}
