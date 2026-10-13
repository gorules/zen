import { spawnSync } from 'node:child_process';
import { readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const packageJsonPath = join(import.meta.dirname, 'package.json');
const readPackageJson = () => JSON.parse(readFileSync(packageJsonPath, 'utf8'));

const before = readPackageJson();

const napi = spawnSync('napi', ['prepublish', ...process.argv.slice(2)], {
  cwd: import.meta.dirname,
  stdio: 'inherit',
  shell: process.platform === 'win32',
});
if (napi.status !== 0) process.exit(napi.status ?? 1);

const after = readPackageJson();
const keptFiles = new Set(before.files ?? []);
const generatedFiles = (after.files ?? []).filter((file) => !keptFiles.has(file));

const restoreField = (target, source, key) => {
  if (source && Object.prototype.hasOwnProperty.call(source, key)) target[key] = source[key];
  else if (target) delete target[key];
};

restoreField(after, before, 'files');
restoreField(after, before, 'exports');
if (after.publishConfig) restoreField(after.publishConfig, before.publishConfig, 'exports');

writeFileSync(packageJsonPath, `${JSON.stringify(after, null, 2)}\n`);

for (const file of generatedFiles) rmSync(join(import.meta.dirname, file), { force: true });
