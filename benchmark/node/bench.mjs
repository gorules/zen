import { readFile } from 'node:fs/promises';
import { readFileSync, writeFileSync } from 'node:fs';
import { hrtime, argv, env } from 'node:process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { ZenEngine } = require('../../bindings/nodejs/index.js');

const dir = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(dir, '..');
const fixtures = path.join(root, 'fixtures');
const manifest = JSON.parse(readFileSync(path.join(root, 'manifest.json'), 'utf8'));
const iters = Number(env.BENCH_ITERS || 2000);
const outPath = argv[2];

const loader = (key) => readFile(path.join(fixtures, key));
const engine = new ZenEngine({ loader });

const results = [];
for (const e of manifest) {
  for (const trace of [false, true]) {
    const opts = { trace };
    await engine.evaluate(e.file, e.input, opts);
    const start = hrtime.bigint();
    for (let i = 0; i < iters; i++) {
      await engine.evaluate(e.file, e.input, opts);
    }
    const per = Number(hrtime.bigint() - start) / iters;
    results.push({ name: `${e.name} (${e.kind})${trace ? ' +trace' : ''}`, unit: 'ns/op', value: per });
  }
}

engine.dispose?.();

if (outPath) {
  writeFileSync(outPath, JSON.stringify(results, null, 2));
} else {
  for (const r of results) {
    console.log(`${r.name.padEnd(44)} ${r.value.toFixed(0).padStart(12)} ns/op`);
  }
}
