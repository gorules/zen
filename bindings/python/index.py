import zen
import asyncio
import unittest

def loader(key):
    with open("../../test-data/" + key, "r") as f:
        return f.read()

def custom_handler(request):
    p1 = request.get_field("prop1")
    return {
        "output": { "sum": p1 }
    }

async def custom_async_handler(request):
    p1 = request.get_field("prop1")
    await asyncio.sleep(1)
    return {
        "output": { "sum": p1 }
    }

# The test based on unittest module
class ZenEngine(unittest.TestCase):
    def test_decision_using_loader(self):
        engine = zen.ZenEngine({"loader": loader})
        r1 = engine.evaluate("function.json", {"input": 5})
        r2 = engine.evaluate("table.json", {"input": 2})
        r3 = engine.evaluate("table.json", {"input": 12})

        self.assertEqual(r1["result"]["output"], 10)
        self.assertEqual(r2["result"]["output"], 0)
        self.assertEqual(r3["result"]["output"], 10)

    def test_decisions_using_getDecision(self):
        engine = zen.ZenEngine({"loader": loader})

        functionDecision = engine.get_decision("function.json")
        tableDecision = engine.get_decision("table.json")

        r1 = functionDecision.evaluate({"input": 10})
        r2 = tableDecision.evaluate({"input": 5})
        r3 = tableDecision.evaluate({"input": 12})

        self.assertEqual(r1["result"]["output"], 20)
        self.assertEqual(r2["result"]["output"], 0)
        self.assertEqual(r3["result"]["output"], 10)

    def test_create_decisions_from_content(self):
        engine = zen.ZenEngine()
        with open("../../test-data/function.json", "r") as f:
            functionContent = f.read()
        functionDecision = engine.create_decision(functionContent)

        r = functionDecision.evaluate({"input": 15})
        self.assertEqual(r["result"]["output"], 30)

    def test_engine_custom_handler(self):
        engine = zen.ZenEngine({ "loader": loader, "customHandler": custom_handler })
        r1 = engine.evaluate("custom.json", {"a": 10})
        r2 = engine.evaluate("custom.json", {"a": 20})
        r3 = engine.evaluate("custom.json", {"a": 30})

        self.assertEqual(r1["result"]["sum"], 20)
        self.assertEqual(r2["result"]["sum"], 30)
        self.assertEqual(r3["result"]["sum"], 40)

    def test_evaluate_expression(self):
        result = zen.evaluate_expression("sum(a)", { "a": [1, 2, 3, 4] })
        self.assertEqual(result, 10)

    def test_evaluate_unary_expression(self):
        result = zen.evaluate_unary_expression("'FR', 'ES', 'GB'", { "$": "GB" })
        self.assertEqual(result, True)

    def test_render_template(self):
        result = zen.render_template("{{ a + b }}", { "a": 10, "b": 20 })
        self.assertEqual(result, 30)


class AsyncZenEngine(unittest.IsolatedAsyncioTestCase):
    async def test_async_evaluate(self):
        engine = zen.ZenEngine({"loader": loader})
        r1 = engine.async_evaluate("function.json", {"input": 5})
        r2 = engine.async_evaluate("table.json", {"input": 2})
        r3 = engine.async_evaluate("table.json", {"input": 12})

        results = await asyncio.gather(r1, r2, r3)
        self.assertEqual(results[0]["result"]["output"], 10)
        self.assertEqual(results[1]["result"]["output"], 0)
        self.assertEqual(results[2]["result"]["output"], 10)

    async def test_async_evaluate_custom_handler(self):
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_async_handler})
        r1 = engine.async_evaluate("custom.json", {"a": 10})
        r2 = engine.async_evaluate("custom.json", {"a": 20})
        r3 = engine.async_evaluate("custom.json", {"a": 30})

        results = await asyncio.gather(r1, r2, r3)
        self.assertEqual(results[0]["result"]["sum"], 20)
        self.assertEqual(results[1]["result"]["sum"], 30)
        self.assertEqual(results[2]["result"]["sum"], 40)

    async def test_create_decisions_from_content(self):
        engine = zen.ZenEngine()
        with open("../../test-data/function.json", "r") as f:
            functionContent = f.read()
        functionDecision = engine.create_decision(functionContent)

        r = await functionDecision.async_evaluate({"input": 15})
        self.assertEqual(r["result"]["output"], 30)


# run the test
unittest.main()
