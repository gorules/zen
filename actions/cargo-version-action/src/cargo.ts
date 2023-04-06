import * as path from 'path';
import * as toml from "toml";

const escapeDir = (count: number) => '../'.repeat(count);
export const coreDirectory = path.join(__dirname, escapeDir(3), 'core');

type UpdateCargoOptions = {
  version: string;
}

const versionRegex = /version = "[0-9]+\.[0-9]+\.[0-9]+"$/mi;
const vmDep = /zen-vm =.*$/mi;
const parserDep = /zen-parser =.*$/mi;

export const updateCargoContents = (contents: string, {version}: UpdateCargoOptions): string => {
  return contents
    .replace(versionRegex, `version = "${version}"`)
    .replace(parserDep, `zen-parser = { path = "../parser", version = "${version}" }`)
    .replace(vmDep, `zen-vm = { path = "../vm", version = "${version}" }`);
}

export const getCargoVersion = (contents: string): string => {
  const t = toml.parse(contents);
  return t?.package?.version;
}