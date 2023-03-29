import * as core from '@actions/core';
import * as exec from '@actions/exec';

import * as fs from 'fs';
import * as toml from 'toml';
import * as path from 'path';
import { inc, ReleaseType } from 'semver';

const versionRegex = /version = "[0-9]+\.[0-9]+\.[0-9]+"/i;

async function run() {
  try {
    const versionBump = core.getInput('version') || 'patch';
    const tagPrefix = core.getInput('tag-prefix') || 'v';
    const commitMessage = core.getInput('commit-message') || 'chore(release): publish core';

    const workspace = (process?.env?.GITHUB_WORKSPACE as string) || '../../';
    const project = 'engine';
    const projectsFolder = 'core';

    const folders = fs
      .readdirSync(path.join(...[workspace, projectsFolder]), { withFileTypes: true })
      .filter((dirent) => dirent.isDirectory())
      .map((dirent) => dirent.name);

    const cargoFilePath = path.join(...[workspace, projectsFolder, project, 'Cargo.toml']);
    const cargoFile = fs.readFileSync(cargoFilePath, 'utf-8');

    const parsed = toml.parse(cargoFile);
    const currentVersion = parsed?.package?.version;
    console.log(`Reading current version ${currentVersion}, from ${cargoFilePath}`);

    const version = inc(currentVersion, versionBump as ReleaseType);

    console.log(`New version: ${version}`);
    const files: string[] = [];
    await Promise.all(
      folders.map(async (folder) => {
        const cargoFilePath = path.join(...[workspace, projectsFolder, folder, 'Cargo.toml']);
        const cargoFile = fs.readFileSync(cargoFilePath, 'utf-8').replace(versionRegex, `version = "${version}"`);
        console.log(`Writing new version to: ${cargoFilePath}`);
        fs.writeFileSync(cargoFilePath, cargoFile);
        files.push(path.join(projectsFolder, folder, 'Cargo.toml'));
      }),
    );
    const tag = `${tagPrefix}${version}`;
    const tagArgs = ['tag', '-a', tag];
    if (commitMessage) {
      tagArgs.push('-m');
      tagArgs.push(`"${commitMessage}"`);
    }
    await exec.exec('git', tagArgs);
    await exec.exec('git', ['add', '.']);
    await exec.exec('git', ['commit', '-m', `"${commitMessage}"`]);
    console.log(`New tag ${tag}`);
    core.setOutput('version', version);
    core.setOutput('tag', tag);
  } catch (error) {
    core.setFailed(error.message);
  }
}

run();
