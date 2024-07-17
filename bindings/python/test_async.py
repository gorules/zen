import asyncio
import unittest

import zen


def loader(key):
    with open("../../test-data/" + key, "r") as f:
        return f.read()


def custom_handler(request):
    p1 = request.get_field("prop1")
    return {
        "output": {"sum": p1}
    }


async def custom_async_handler(request):
    p1 = request.get_field("prop1")
    await asyncio.sleep(0.25)
    return {
        "output": {"sum": p1}
    }


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


if __name__ == '__main__':
    unittest.main()
