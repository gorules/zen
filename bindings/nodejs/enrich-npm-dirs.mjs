import { readdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const npmRoot = join(import.meta.dirname, 'npm');

for (const dir of readdirSync(npmRoot)) {
  const readmePath = join(npmRoot, dir, 'README.md');
  if (!existsSync(readmePath)) continue;
  const readme = readFileSync(readmePath, 'utf8');
  const target = readme.match(/This is the \*\*(.+?)\*\* binary/)?.[1] ?? dir;
  writeFileSync(
    readmePath,
    `# \`@gorules/zen-engine-${dir}\`

This is the **${target}** binary for [\`@gorules/zen-engine\`](https://www.npmjs.com/package/@gorules/zen-engine), the open-source [Node.js rules engine](https://gorules.io/open-source/javascript-rules-engine) from [GoRules](https://gorules.io).

- [Documentation](https://docs.gorules.io/developers/sdks/nodejs)
- [GitHub](https://github.com/gorules/zen)
`,
  );
}
