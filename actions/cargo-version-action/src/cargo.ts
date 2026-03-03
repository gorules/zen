import * as toml from 'toml';

type UpdateCargoOptions = {
  version: string;
};

const versionRegex = /version = "[0-9]+\.[0-9]+\.[0-9]+"$/im;
const zenDepLine = /^.*zen-(?:expression|tmpl|macros|types)\s*=\s*\{.*}.*$/gim;
const depVersionRegex = /version\s*=\s*"[0-9]+\.[0-9]+\.[0-9]+"/;

export const updateCargoContents = (contents: string, { version }: UpdateCargoOptions): string => {
  return contents
    .replace(versionRegex, `version = "${version}"`)
    .replace(zenDepLine, (line) => line.replace(depVersionRegex, `version = "${version}"`));
};

export const getCargoVersion = (contents: string): string => {
  const t = toml.parse(contents);
  return t?.package?.version;
};
