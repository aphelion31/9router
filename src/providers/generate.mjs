// Regenerate snapshots from the sibling JavaScript implementation.
import registry from '../../../9router-pro/open-sse/providers/registry/index.js';
import { TABLES } from '../../../9router-pro/src/lib/db/schema.js';
import { writeFileSync, readFileSync } from 'node:fs';
const data = registry.map(p => ({
  id: p.id, alias: p.alias || p.id,
  base_url: p.transport?.baseUrl ?? null,
  format: p.transport ? (p.transport.format || 'openai') : null,
  auth_type: p.category ?? null,
  models: p.models ?? [],
}));
for (const [file, value] of [['registry.json', data], ['schema.json', TABLES]]) {
  const path = new URL(`./${file}`, import.meta.url);
  const contents = JSON.stringify(value, null, 2) + '\n';
  if (process.argv.includes('--check')) {
    if (readFileSync(path, 'utf8') !== contents) throw new Error(`${file} differs from upstream`);
  } else writeFileSync(path, contents);
}
