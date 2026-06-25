import { execSync, execFileSync } from 'node:child_process';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(dir, '..');
const manifest = JSON.parse(readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
const rowNames = manifest.flatMap((e) => [`${e.name} (${e.kind})`, `${e.name} (${e.kind}) +trace`]);

const langs = [
  { key: 'rust', build: 'build-rust', cmd: path.join(root, 'target', 'release', 'zen-benchmark'), args: ['manifest.json'] },
  { key: 'node', build: 'build-node', cmd: process.execPath, args: [path.join(dir, 'node', 'bench.mjs')] },
  { key: 'python', build: 'build-python', cmd: path.join(dir, '.venv', 'bin', 'python'), args: [path.join(dir, 'python', 'bench.py')] },
];

const tmp = mkdtempSync(path.join(tmpdir(), 'zen-bench-'));
const values = {};
for (const lang of langs) {
  try {
    execSync(`make ${lang.build}`, { cwd: dir, stdio: 'inherit', env: process.env });
    const out = path.join(tmp, `${lang.key}.json`);
    execFileSync(lang.cmd, [...lang.args, out], { cwd: dir, stdio: ['ignore', 'ignore', 'inherit'], env: process.env });
    const arr = JSON.parse(readFileSync(out, 'utf8'));
    values[lang.key] = Object.fromEntries(arr.map((e) => [e.name, e.value]));
  } catch (err) {
    console.error(`! skipping ${lang.key}: ${err.message}`);
    values[lang.key] = null;
  }
}
rmSync(tmp, { recursive: true, force: true });

const NAME_W = 52;
const COL_W = 22;
const get = (key, name) => (values[key] ? values[key][name] : undefined);
const cell = (v, base) => {
  if (v == null) return 'n/a'.padStart(COL_W);
  const ns = `${Math.round(v)} ns`;
  if (base == null) return ns.padStart(COL_W);
  const mult = base === 0 ? '' : `${(v / base).toFixed(2)}x`;
  return `${ns}  ${mult}`.padStart(COL_W);
};

const header = `${'fixture'.padEnd(NAME_W)}${['rust', 'node', 'python'].map((l) => l.padStart(COL_W)).join('')}`;
console.log('');
console.log(header);
console.log('-'.repeat(header.length));
for (const name of rowNames) {
  const r = get('rust', name);
  console.log(`${name.padEnd(NAME_W)}${cell(r, null)}${cell(get('node', name), r)}${cell(get('python', name), r)}`);
}
console.log('');
console.log('x = slowdown vs rust (rust is the baseline floor; lower is better)');
