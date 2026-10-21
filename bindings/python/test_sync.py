import json
import os.path
import unittest
import glob
import os

import zen

os.environ['__ZEN_MOCK_UTC_TIME'] = '2025-08-19T16:55:02.078Z'

def loader(key):
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

def http_handler_decision_content():
    source = (
        "import http from 'http';\n"
        "\n"
        "export const handler = async (input) => {\n"
        "  const response = await http.get('https://example.com/products/1', {\n"
        "    headers: { 'x-request': 'ping' },\n"
        "    params: { page: '1' },\n"
        "  });\n"
        "\n"
        "  return {\n"
        "    status: response.status,\n"
        "    product: response.data.product,\n"
        "    mockHeader: response.headers['x-mock'],\n"
        "  };\n"
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
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_handler})
        r1 = engine.evaluate("custom.json", {"a": 10})
        r2 = engine.evaluate("custom.json", {"a": 20})
        r3 = engine.evaluate("custom.json", {"a": 30})

        self.assertEqual(r1["result"]["sum"], 20)
        self.assertEqual(r2["result"]["sum"], 30)
        self.assertEqual(r3["result"]["sum"], 40)

    def test_engine_http_handler(self):
        requests = []

        def http_handler(request):
            requests.append(request)
            return {
                "status": 200,
                "headers": {"x-mock": "true"},
                "data": {"product": "notebook"},
            }

        engine = zen.ZenEngine({"httpHandler": http_handler})
        decision = engine.create_decision(http_handler_decision_content())
        r = decision.evaluate({})

        self.assertEqual(r["result"]["status"], 200)
        self.assertEqual(r["result"]["product"], "notebook")
        self.assertEqual(r["result"]["mockHeader"], "true")

        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0]["method"], "GET")
        self.assertEqual(requests[0]["url"], "https://example.com/products/1")
        self.assertEqual(requests[0]["headers"]["x-request"], "ping")
        self.assertEqual(requests[0]["params"]["page"], "1")

    def test_engine_http_handler_error(self):
        def http_handler(request):
            raise PermissionError("domain not allowed")

        engine = zen.ZenEngine({"httpHandler": http_handler})
        decision = engine.create_decision(http_handler_decision_content())

        with self.assertRaises(RuntimeError) as ctx:
            decision.evaluate({})

        self.assertIn("domain not allowed", str(ctx.exception))

    def test_static_loader_config(self):
        with open("../../test-data/table.json", "r") as f:
            table_content = json.loads(f.read())

        engine = zen.ZenEngine({"loader": {"type": "static", "content": {"table.json": table_content}}})
        r1 = engine.evaluate("table.json", {"input": 2})
        r2 = engine.evaluate("table.json", {"input": 12})

        self.assertEqual(r1["result"]["output"], 0)
        self.assertEqual(r2["result"]["output"], 10)
        self.assertRaises(RuntimeError, engine.evaluate, "missing.json", {})

    def test_fs_loader_config(self):
        engine = zen.ZenEngine({"loader": {"type": "fs", "path": "../../test-data"}})
        r1 = engine.evaluate("table.json", {"input": 2})
        r2 = engine.evaluate("table.json", {"input": 12})

        self.assertEqual(r1["result"]["output"], 0)
        self.assertEqual(r2["result"]["output"], 10)

    def test_zip_loader_config(self):
        import io
        import zipfile

        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
            with open("../../test-data/table.json", "rb") as f:
                archive.writestr("table.json", f.read())

        engine = zen.ZenEngine({"loader": {"type": "zip", "bytes": buffer.getvalue()}})
        r1 = engine.evaluate("table.json", {"input": 2})
        r2 = engine.evaluate("table.json", {"input": 12})

        self.assertEqual(r1["result"]["output"], 0)
        self.assertEqual(r2["result"]["output"], 10)

    def test_evaluate_batch(self):
        engine = zen.ZenEngine({"loader": {"type": "fs", "path": "../../test-data"}})
        results = engine.evaluate_batch([
            {"key": "table.json", "context": {"input": 12}},
            {"key": "missing.json", "context": {}},
            {"key": "table.json", "context": {"input": 5}},
        ])

        self.assertEqual(len(results), 3)
        self.assertTrue(results[0]["success"])
        self.assertEqual(results[0]["data"]["result"]["output"], 10)
        self.assertFalse(results[1]["success"])
        self.assertIn("error", results[1])
        self.assertTrue(results[2]["success"])
        self.assertEqual(results[2]["data"]["result"]["output"], 0)

    def test_evaluate_batch_empty(self):
        engine = zen.ZenEngine({"loader": loader})
        self.assertEqual(engine.evaluate_batch([]), [])

    def test_evaluate_expression(self):
        result = zen.evaluate_expression("sum(a)", {"a": [1, 2, 3, 4]})
        self.assertEqual(result, 10)

    def test_evaluate_unary_expression(self):
        result = zen.evaluate_unary_expression("'FR', 'ES', 'GB'", {"$": "GB"})
        self.assertEqual(result, True)

    def test_render_template(self):
        result = zen.render_template("{{ a + b }}", {"a": 10, "b": 20})
        self.assertEqual(result, 30)

    def test_sleep_function(self):
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_handler})

        engine.evaluate("sleep-function.json", {})
        self.assertTrue(True)

    def test_http_function(self):
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_handler})

        engine.evaluate("http-function.json", {})
        self.assertTrue(True)

    def test_additional_options(self):
        engine = zen.ZenEngine({"loader": loader, "customHandler": custom_handler})

        engine.evaluate("sleep-function.json", {}, {"trace": True})
        self.assertTrue(True)

    def test_evaluate_graphs(self):
        engine = zen.ZenEngine({"loader": graph_loader})
        json_files = glob.glob("../../test-data/graphs/*.json")

        for json_file in json_files:
            with open(json_file, "r") as f:
                json_contents = json.loads(f.read())

            for test_case in json_contents["tests"]:
                key = os.path.basename(json_file)

                engine_response = engine.evaluate(key, test_case["input"])
                decision = engine.get_decision(key)
                decision_response = decision.evaluate(test_case["input"])

                self.assertEqual(engine_response["result"], test_case["output"], key)
                self.assertEqual(decision_response["result"], test_case["output"], key)

if __name__ == '__main__':
    unittest.main()
