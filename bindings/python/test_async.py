import asyncio
import glob
import json
import os.path
import time
import unittest
import os

import zen

os.environ['__ZEN_MOCK_UTC_TIME'] = '2025-08-19T16:55:02.078Z'

async def loader(key):
    with open("../../test-data/" + key, "r") as f:
        return f.read()


def graph_loader(key):
    with open("../../test-data/graphs/" + key, "r") as f:
        return f.read()


def custom_handler(request):
    p1 = request.get_field("prop1")
    return {
        "output": {"sum": p1}
    }


async def custom_async_handler(request):
    p1 = request.get_field("prop1")
    await asyncio.sleep(0.1)
    return {
        "output": {"sum": p1}
    }


def http_handler_decision_content():
    source = (
        "import http from 'http';\n"
        "\n"
        "export const handler = async (input) => {\n"
        "  const response = await http.get('https://example.com/products/1');\n"
        "  return { status: response.status, product: response.data.product };\n"
        "};\n"
    )
    return json.dumps({
        "contentType": "application/vnd.gorules.decision",
        "nodes": [
            {"type": "inputNode", "id": "input1", "name": "request", "position": {"x": 0, "y": 0}},
            {"type": "functionNode", "id": "function1", "name": "function1",
             "content": {"source": source}, "position": {"x": 100, "y": 0}},
            {"type": "outputNode", "id": "output1", "name": "response", "position": {"x": 200, "y": 0}},
        ],
        "edges": [
            {"id": "edge1", "type": "edge", "sourceId": "input1", "targetId": "function1"},
            {"id": "edge2", "type": "edge", "sourceId": "function1", "targetId": "output1"},
        ],
    })


class AsyncZenEngine(unittest.IsolatedAsyncioTestCase):
    async def test_async_http_handler(self):
        async def http_handler(request):
            await asyncio.sleep(0.1)
            return {
                "status": 200,
                "headers": {},
                "data": {"product": "notebook"},
            }

        engine = zen.ZenEngine({"httpHandler": http_handler})
        decision = engine.create_decision(http_handler_decision_content())
        r = await decision.async_evaluate({})

        self.assertEqual(r["result"]["status"], 200)
        self.assertEqual(r["result"]["product"], "notebook")

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

    async def test_async_sleep_function(self):
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_async_handler})

        await engine.async_evaluate("sleep-function.json", {})
        self.assertTrue(True)

    async def test_async_http_function(self):
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_async_handler})

        await engine.async_evaluate("http-function.json", {})
        self.assertTrue(True)

    async def test_create_decisions_from_content(self):
        engine = zen.ZenEngine()
        with open("../../test-data/function.json", "r") as f:
            functionContent = f.read()
        functionDecision = engine.create_decision(functionContent)

        r = await functionDecision.async_evaluate({"input": 15})
        self.assertEqual(r["result"]["output"], 30)

    async def test_evaluate_graphs(self):
        engine = zen.ZenEngine({"loader": graph_loader})
        json_files = glob.glob("../../test-data/graphs/*.json")

        for json_file in json_files:
            with open(json_file, "r") as f:
                json_contents = json.loads(f.read())

            for test_case in json_contents["tests"]:
                key = os.path.basename(json_file)

                engine_response = await engine.async_evaluate(key, test_case["input"])
                decision = engine.get_decision(key)
                decision_response = await decision.async_evaluate(test_case["input"])

                self.assertEqual(engine_response["result"], test_case["output"])
                self.assertEqual(decision_response["result"], test_case["output"])


if __name__ == '__main__':
    unittest.main()
