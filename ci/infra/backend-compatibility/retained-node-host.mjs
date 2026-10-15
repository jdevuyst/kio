import assert from 'node:assert/strict';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const artifact = process.argv[2];
const { createFfiExportPolyRoundtrip } = await import(
  pathToFileURL(path.join(artifact, 'ffi_export_poly_roundtrip.js')).href
);
const { testapi } = createFfiExportPolyRoundtrip();
const values = [
  testapi.polyEcho('poly-string'),
  testapi.polyEcho(42),
  testapi.keepLeft('left', true),
];
assert.deepEqual(values, ['poly-string', 42, 'left']);
console.log(values.join('\n'));
