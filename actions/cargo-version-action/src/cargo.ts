import * as toml from 'toml';

type UpdateCargoOptions = {
  version: string;
};

const versionRegex = /version = "[0-9]+\.[0-9]+\.[0-9]+"$/im;
const expressionDep = /zen-expression =.*$/im;
const templateDep = /zen-template =.*$/im;

export const updateCargoContents = (contents: string, { version }: UpdateCargoOptions): string => {
  return contents
    .replace(versionRegex, `version = "${version}"`)
    .replace(expressionDep, `zen-expression = { path = "../expression", version = "${version}" }`)
    .replace(templateDep, `zen-template = { path = "../template", version = "${version}" }`);
};

export const getCargoVersion = (contents: string): string => {
  const t = toml.parse(contents);
  return t?.package?.version;
};
