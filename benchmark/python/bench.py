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
    engine.evaluate(e["file"], e["input"])
    start = time.perf_counter_ns()
    for _ in range(iters):
        engine.evaluate(e["file"], e["input"])
    per = (time.perf_counter_ns() - start) / iters
    results.append({"name": f'{e["name"]} ({e["kind"]})', "unit": "ns/op", "value": per})

out = json.dumps(results, indent=2)
if out_path:
    with open(out_path, "w") as f:
        f.write(out)
else:
    print(out)
