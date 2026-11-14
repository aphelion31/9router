// Refresh provider metadata from the adjacent production source checkout.
import fs from 'node:fs';
import { pathToFileURL } from 'node:url';
const root = process.argv[2];
if (!root) throw new Error('Usage: node scripts/refresh-provider-aliases.mjs /path/to/9router-pro');
const registry = (await import(pathToFileURL(`${root}/open-sse/providers/registry/index.js`))).default;
const byId = new Map(registry.map(p => [p.id, p]));
const path = new URL('../src/providers/registry.json', import.meta.url);
const snapshot = JSON.parse(fs.readFileSync(path, 'utf8'));
for (const p of snapshot) {
  const upstream = byId.get(p.id);
  if (!upstream) continue;
  p.aliases = upstream.aliases || [];
  p.uiAlias = upstream.uiAlias || upstream.alias || p.id;
  p.noAuthFree = upstream.category === 'free' && upstream.noAuth === true;
}
fs.writeFileSync(path, `${JSON.stringify(snapshot, null, 2)}\n`);
