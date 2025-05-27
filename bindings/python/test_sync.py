import json
import os.path
import unittest
import glob

import zen


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

                self.assertEqual(engine_response["result"], test_case["output"])
                self.assertEqual(decision_response["result"], test_case["output"])

if __name__ == '__main__':
    unittest.main()
