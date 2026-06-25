import json
import os
import sys
import time

import zen

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
fixtures = os.path.join(root, "fixtures")
manifest = json.load(open(os.path.join(root, "manifest.json")))
iters = int(os.environ.get("BENCH_ITERS", "2000"))
out_path = sys.argv[1] if len(sys.argv) > 1 else None


def loader(key):
    with open(os.path.join(fixtures, key)) as f:
        return f.read()


engine = zen.ZenEngine({"loader": loader})

results = []
for e in manifest:
    for trace in (False, True):
        opts = {"trace": trace}
        engine.evaluate(e["file"], e["input"], opts)
        start = time.perf_counter_ns()
        for _ in range(iters):
            engine.evaluate(e["file"], e["input"], opts)
        per = (time.perf_counter_ns() - start) / iters
        suffix = " +trace" if trace else ""
        results.append({"name": f'{e["name"]} ({e["kind"]}){suffix}', "unit": "ns/op", "value": per})

if out_path:
    with open(out_path, "w") as f:
        f.write(json.dumps(results, indent=2))
else:
    for r in results:
        print(f'{r["name"]:<44} {r["value"]:>12.0f} ns/op')
