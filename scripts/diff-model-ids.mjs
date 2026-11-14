#!/usr/bin/env node
// Compare ordered IDs without exposing API credentials.
import fs from 'node:fs';
const [oracleFile, actualFile] = process.argv.slice(2);
if (!oracleFile || !actualFile) throw new Error('usage: node scripts/diff-model-ids.mjs oracle.json actual.json');
const ids = f => JSON.parse(fs.readFileSync(f, 'utf8')).data.map(m => m.id);
const expected = ids(oracleFile), actual = ids(actualFile);
const missing = expected.filter(id => !actual.includes(id));
const extra = actual.filter(id => !expected.includes(id));
const first = expected.findIndex((id, i) => actual[i] !== id);
console.log(`oracle=${expected.length} rust=${actual.length} missing=${missing.length} extra=${extra.length} order=${first < 0 && expected.length === actual.length ? 'match' : `diff at ${first}`}`);
if (missing.length) console.log('missing:', missing.join(', '));
if (extra.length) console.log('extra:', extra.join(', '));
if (missing.length || extra.length || first >= 0 || expected.length !== actual.length) process.exitCode = 1;
