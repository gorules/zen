# Zen Cargo action

This action bumps the Cargo version of the core Engine.

## Inputs

### `version`

**Required** The bump of the version, one of `"patch"`, `"minor"`, `"major"`. Default `"patch"`.

### `tag-prefix`

**Optional** The prefix of the tag. Default `"v"`.

## Outputs

### `version`

The new version in the form of `x.x.x`.

## Developmentw
```bash
npm i
#dev
npm run dev

#build
npm run build
```

