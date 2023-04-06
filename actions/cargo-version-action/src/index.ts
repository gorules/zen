import * as core from '@actions/core';
import * as exec from '@actions/exec';

import * as fs from 'fs/promises';
import * as path from 'path';
import {inc, ReleaseType} from 'semver';
import {getCargoVersion, updateCargoContents} from "./cargo";

async function run() {
  try {
    const versionBump = core.getInput('version') || 'patch';
    const tagPrefix = core.getInput('tag-prefix') || 'v';
    const commitMessage = core.getInput('commit-message') || 'chore(release): publish core';

    const workspace = (process?.env?.GITHUB_WORKSPACE as string) || '../../';
    const project = 'engine';
    const projectsFolder = 'core';

    const directory = await fs.readdir(path.join(...[workspace, projectsFolder]), {withFileTypes: true});
    const folders = directory
      .filter((dirent) => dirent.isDirectory())
      .map((dirent) => dirent.name);

    const cargoFilePath = path.join(...[workspace, projectsFolder, project, 'Cargo.toml']);
    const cargoFile = await fs.readFile(cargoFilePath, 'utf-8');
    const currentVersion = getCargoVersion(cargoFile);
    console.log(`Reading current version ${currentVersion}, from ${cargoFilePath}`);

    const version = inc(currentVersion, versionBump as ReleaseType);
    console.log(`New version: ${version}`);

    await Promise.all(
      folders.map(async (folder) => {
        const cargoFilePath = path.join(...[workspace, projectsFolder, folder, 'Cargo.toml']);
        const cargoFile = await fs.readFile(cargoFilePath, 'utf-8');
        const updatedCargoFile = updateCargoContents(cargoFile, {version});

        console.log(`Writing new version to: ${cargoFilePath}`);
        await fs.writeFile(cargoFilePath, updatedCargoFile);
      }),
    );

    const tag = `${tagPrefix}${version}`;
    const tagArgs = ['tag', '-a', tag];
    if (commitMessage) {
      tagArgs.push('-m');
      tagArgs.push(commitMessage);
    }

    await exec.exec('git', tagArgs);
    await exec.exec('git', ['add', '.']);
    await exec.exec('git', ['commit', '-m', commitMessage]);
    console.log(`New tag ${tag}`);

    core.setOutput('version', version);
    core.setOutput('tag', tag);
  } catch (error) {
    core.setFailed(error.message);
  }
}

run();
