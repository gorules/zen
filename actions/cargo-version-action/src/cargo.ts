import * as toml from 'toml';

type UpdateCargoOptions = {
  version: string;
};

const versionRegex = /version = "[0-9]+\.[0-9]+\.[0-9]+"$/im;
const expressionDep = /zen-expression =.*$/im;
const templateDep = /zen-tmpl =.*$/im;
const macroDep = /zen-macros =.*$/im;
const typesDep = /zen-types =.*$/im;

export const updateCargoContents = (contents: string, { version }: UpdateCargoOptions): string => {
  return contents
    .replace(versionRegex, `version = "${version}"`)
    .replace(expressionDep, `zen-expression = { path = "../expression", version = "${version}" }`)
    .replace(macroDep, `zen-macros = { path = "../macros", version = "${version}" }`)
    .replace(typesDep, `zen-types = { path = "../types", version = "${version}" }`)
    .replace(templateDep, `zen-tmpl = { path = "../template", version = "${version}" }`);
};

export const getCargoVersion = (contents: string): string => {
  const t = toml.parse(contents);
  return t?.package?.version;
};
