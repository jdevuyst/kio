#!/bin/sh
# SUBJECT: Recursive-group effect bodies stay single-copy in emitted java source.
# CONTRACT: Measured recursive-dispatcher body sharing.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-rec-group.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

find "out/$KIO_TARGET" -type f -name '*.java' -exec cat {} + > "$scratch/emitted"
node --input-type=module - "$scratch/emitted" <<'NODE'
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

const source = readFileSync(process.argv[2], 'utf8');
const builders = [...source.matchAll(/^  private static String buildKioIrJson\(\) \{\r?\n([\s\S]*?)^  \}/gm)];
assert.equal(builders.length, 1, 'exactly one complete IR builder');
const lines = builders[0][1].trim().split(/\r?\n/).map(line => line.trim());
const capacity = lines.shift().match(/^StringBuilder out = new StringBuilder\((\d+)\);$/);
assert.ok(capacity, 'IR builder capacity declaration');
assert.equal(lines.pop(), 'return out.toString();');
assert.ok(lines.length > 0, 'IR builder contains string chunks');
// Java's emitted quote, backslash and control escapes are JSON-compatible.
const json = lines.map(line => {
  const append = line.match(/^out\.append\(("(?:\\.|[^"\\])*")\);$/);
  assert.ok(append, 'each IR builder line appends one encoded string');
  return JSON.parse(append[1]);
}).join('');
assert.equal(Buffer.byteLength(json, 'utf8'), Number(capacity[1]));
const counts = new Map([
  'shared-first-effect', 'shared-second-before', 'shared-second-after',
  'shared-third-effect', 'shared-fourth-effect',
].map(payload => [payload, 0]));
const pending = [JSON.parse(json)];
while (pending.length > 0) {
  const value = pending.pop();
  if (value === null || typeof value !== 'object') continue;
  const payload = value.StrLit?.value;
  if (counts.has(payload)) counts.set(payload, counts.get(payload) + 1);
  pending.push(...Object.values(value));
}
for (const [payload, count] of counts) {
  assert.equal(count, 1, `${payload} has ${count} emitted copies; the measured shared-body count is 1`);
}
NODE
