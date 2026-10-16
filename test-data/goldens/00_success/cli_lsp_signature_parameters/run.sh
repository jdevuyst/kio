#!/bin/sh
# Selected public-type packets determine which source-spelled parameter
# regions ordinary signature help highlights, including product packing.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp workdir/*.kio "$work/"
cd "$work"
for signature_offsets in false true; do
SIGNATURE_OFFSETS="$signature_offsets" node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const source = readFileSync('main.kio', 'utf8');
const uri = pathToFileURL(resolve('main.kio')).href;
const provider = readFileSync('provider.kio', 'utf8');
const providerUri = pathToFileURL(resolve('provider.kio')).href;
const offsets = process.env.SIGNATURE_OFFSETS === 'true';
const child = spawn(process.env.KIO_BIN, ['lsp'], { stdio: ['pipe', 'pipe', 'inherit'] });
const closed = new Promise(resolve => child.on('close', resolve));
const hardTimeout = setTimeout(() => child.kill('SIGKILL'), 120000);
let buffer = Buffer.alloc(0), nextId = 1;
const pending = new Map();
function send(method, params, id) {
  const body = JSON.stringify({ jsonrpc: '2.0', method, params, ...(id ? { id } : {}) });
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}
function request(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`timeout: ${method}`)), 30000);
    pending.set(id, message => { clearTimeout(timeout); message.error ? reject(message.error) : resolve(message.result); });
    send(method, params, id);
  });
}
child.stdout.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd < 0) return;
    const length = Number(/Content-Length: (\d+)/i.exec(buffer.subarray(0, headerEnd).toString())[1]);
    const end = headerEnd + 4 + length;
    if (buffer.length < end) return;
    const message = JSON.parse(buffer.subarray(headerEnd + 4, end));
    buffer = buffer.subarray(end);
    if (message.id) { pending.get(message.id)?.(message); pending.delete(message.id); }
  }
});
try {
  await request('initialize', {
    processId: process.pid, rootUri: pathToFileURL(resolve('.')).href,
    capabilities: offsets ? { textDocument: { signatureHelp: { signatureInformation: {
      parameterInformation: { labelOffsetSupport: true }, activeParameterSupport: true,
    } } } } : {},
  });
  send('initialized', {});
  send('textDocument/didOpen', { textDocument: { uri, languageId: 'kio', version: 1, text: source } });
  const line = source.split('\n').findIndex(text => text.startsWith('pub fn run('));
  const character = source.split('\n')[line].lastIndexOf('()') + 1;
  let result = null;
  for (let attempt = 0; attempt < 200 && result === null; ++attempt) {
    result = await request('textDocument/signatureHelp', { textDocument: { uri }, position: { line, character } });
    if (result === null) await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.notEqual(result, null);
  const signature = result.signatures[0];
  const index = signature.activeParameter ?? result.activeParameter;
  assert.ok(Array.isArray(signature.parameters), 'active value parameters must be highlightable');
  assert.ok(index >= 0 && index < signature.parameters.length, 'active parameter must be present');
  const region = signature.parameters[index].label;
  const text = Array.isArray(region) ? signature.label.slice(...region) : region;
  assert.equal(text, 'right: A');
  function position(name, needle, occurrence = 0) {
    let start = source.indexOf('{', source.indexOf(`pub fn ${name}(`));
    assert.ok(start >= 0, name);
    for (let n = 0; n <= occurrence; ++n) {
      start = source.indexOf(needle, start + 1);
      assert.ok(start >= 0, `${name}: ${needle}`);
    }
    const before = source.slice(0, start);
    return { textDocument: { uri }, position: {
      line: before.split('\n').length - 1, character: before.length - before.lastIndexOf('\n'),
    } };
  }
  function activeText(result, expected, name) {
    assert.notEqual(result, null, name);
    const signature = result.signatures[0];
    assert.ok(Array.isArray(signature.parameters), name);
    const active = result.activeParameter;
    if (expected === null) {
      assert.deepEqual(signature.parameters, [], name);
      assert.equal(active, undefined, name);
      assert.equal(signature.activeParameter, undefined, name);
      return;
    }
    assert.equal(signature.activeParameter, active, name);
    assert.ok(active >= 0 && active < signature.parameters.length, name);
    for (const parameter of signature.parameters) {
      assert.equal(Array.isArray(parameter.label), offsets, `${name}: negotiated label representation`);
    }
    const region = signature.parameters[active].label;
    const text = Array.isArray(region) ? signature.label.slice(...region) : region;
    assert.equal(text, expected, name);
    assert.ok(signature.label.includes(text), name);
  }
  const controls = [
    ['split_product', '()', 0, 'packet: . & .'],
    ['split_product', '()', 1, 'packet: . & .'],
    ['whole_packet', '((), ())', 0, 'left: ., right: .'],
    ['split_tail', '()', 0, 'left: .'],
    ['split_tail', '()', 1, 'rest: . & .'],
    ['split_tail', '()', 2, 'rest: . & .'],
    ['packed_tail', '((), ())', 0, 'rest: . & .'],
    ['nested_left', '((), ())', 0, 'first: . & .'],
    ['nested_left', '()', 2, 'last: .'],
    ['multiple_stages', '()', 1, 'packet: . & .'],
    ['multiple_stages', '()', 2, 'next: .'],
    ['later_type', '()', 1, 'second: B'],
    ['later_type', '.,', 0, 'second: B'],
    ['explicit_product', '. & .', 0, 'packet: A'],
    ['explicit_product', '()', 1, 'packet: A'],
    ['inferred_product', '((), ())', 0, 'packet: A'],
    ['alias_product', '()', 1, 'packet: Pair'],
    ['higher_product', ', Box.mk_box', 1, 'packet: Higher(F)'],
    ['returned_stage', '()', 0, '()'],
    ['returned_stage', '()', 1, '. -> .'],
    ['implicit_unit', '()', 0, 'value: .'],
    ['explicit_unit', '()', 0, 'value: .'],
    ['empty_unit', '()', 0, '()'],
    ['only_type', '.', 0, null],
    ['trailing_type', '()', 0, 'value: .'],
    ['trailing_type', '.', 0, null],
    ['trailing_type', ', .', 0, null],
    ['trailing_type', ')', 1, null],
    ['residual_type', '.', 0, 'value: A'],
    ['placeholder', '_', 0, 'left: A'],
    ['placeholder', '()', 1, 'right: A'],
    ['same_labels', '()', 0, 'value: .'],
    ['same_labels', '()', 1, 'value: .'],
    ['host_call', '()', 1, 'right: .'],
    ['remote_call', '()', 1, 'right: .'],
  ];
  for (const [name, needle, occurrence, expected] of controls) {
    activeText(await request('textDocument/signatureHelp', position(name, needle, occurrence)), expected, name);
  }
  if (offsets) {
    const first = (await request('textDocument/signatureHelp', position('same_labels', '()', 0))).signatures[0];
    const second = (await request('textDocument/signatureHelp', position('same_labels', '()', 1))).signatures[0];
    const firstStart = first.label.indexOf('value: .');
    const secondStart = first.label.indexOf('value: .', firstStart + 1);
    assert.deepEqual(first.parameters[first.activeParameter].label, [firstStart, firstStart + 'value: .'.length]);
    assert.deepEqual(second.parameters[second.activeParameter].label, [secondStart, secondStart + 'value: .'.length]);
  }
  assert.equal(await request('textDocument/signatureHelp', position('local_call', '()', 1)), null);
  const remote = () => request('textDocument/signatureHelp', position('remote_call', '()', 1));
  send('textDocument/didOpen', { textDocument: { uri: providerUri, languageId: 'kio', version: 1,
    text: provider.replace('right: .', 'changed: .').replace('{ right }', '{ missing }') } });
  assert.equal(await remote(), null, 'provider-only invalid edit rejects stale labels immediately');
  send('textDocument/didChange', { textDocument: { uri: providerUri, version: 2 }, contentChanges: [{
    text: provider.replace('right: .', 'changed: .').replace('{ right }', '{ changed }'),
  }] });
  async function waitForLabel(expected) {
    for (let attempt = 0; attempt < 400; ++attempt) {
      const result = await remote();
      if (result !== null) { activeText(result, expected, 'provider refresh'); return; }
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.fail(`current provider label did not arrive: ${expected}`);
  }
  await waitForLabel('changed: .');
  send('textDocument/didClose', { textDocument: { uri: providerUri } });
  await waitForLabel('right: .');
  await request('shutdown', null); send('exit', null); child.stdin.end();
  assert.equal(await closed, 0);
} finally {
  clearTimeout(hardTimeout);
  if (child.exitCode === null) { child.kill('SIGTERM'); await closed; }
}
JS
done
printf '%s\n' 'signature help exposes the active value parameter'
