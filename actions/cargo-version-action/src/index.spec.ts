import { describe } from 'node:test';
import { expect, test } from '@jest/globals';
import { getCargoVersion, updateCargoContents } from './cargo';
import { inc, ReleaseType } from 'semver';
import * as fs from 'fs/promises';
import * as path from 'path';

// language=Toml
const makeToml = ({ version }): string => `
    [package]
    authors = ["GoRules Team <bot@gorules.io>"]
    description = "Business rules engine"
    name = "zen-engine"
    license = "MIT"
    version = "${version}"
    edition = "2021"
    repository = "https://github.com/gorules/zen.git"

    [dependencies]
    async-recursion = "1.0.4"
    anyhow = { workspace = true }
    thiserror = { workspace = true }
    async-trait = { workspace = true }
    serde_json = { workspace = true, features = ["arbitrary_precision"] }
    serde = { version = "1", features = ["derive"] }
    serde_v8 = { version = "0.88.0" }
    once_cell = { version = "1.17.1" }
    futures = "0.3.27"
    v8 = { version = "0.66.0" }
    zen-expression = { path = "../expression", version = "${version}" }
    zen-tmpl = { path = "../template", version = "${version}" }
`;

describe('GitHub Action', () => {
  test('Bumps package', () => {
    const version = '0.2.0';
    const initialToml = makeToml({ version });

    const releases: ReleaseType[] = ['major', 'minor', 'patch'];
    for (const release of releases) {
      const newVersion = inc(version, release);
      const expectedToml = makeToml({ version: newVersion });

      expect(updateCargoContents(initialToml, { version: newVersion })).toEqual(expectedToml);
    }
  });

  test('Extracts package version', () => {
    const versions = ['0.1.0', '0.2.0', '0.3.0'];
    for (const version of versions) {
      const versionedToml = makeToml({ version });
      expect(getCargoVersion(versionedToml)).toEqual(version);
    }
  });

  test('Points to right directory', async () => {
    const escapeDir = (count: number) => '../'.repeat(count);
    const coreDirectory = path.join(__dirname, escapeDir(3), 'core');

    const folders = await fs.readdir(coreDirectory);
    expect(folders).toEqual(expect.arrayContaining(['engine', 'expression']));
  });
});
