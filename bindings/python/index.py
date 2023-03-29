import zen
import time
import unittest

def loader(key):
    with open("../../test-data/" + key, "r") as f:
        return f.read()

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

        print(r1)
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

# run the test
unittest.main()
