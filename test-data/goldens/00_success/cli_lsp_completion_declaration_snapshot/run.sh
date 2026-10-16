#!/bin/sh
# The first discovery already supplies declaration identities when a provider
# body fails. Later edits, alias buffers, invalid headers and removal never
# authorize stale provider declarations or documentation.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp workdir/*.kio "$work/"
cd "$work"
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const source = readFileSync('main.kio', 'utf8');
const provider = readFileSync('provider.kio', 'utf8');
const uri = pathToFileURL(resolve('main.kio')).href;
const providerUri = pathToFileURL(resolve('provider.kio')).href;
const child = spawn(process.env.KIO_BIN, ['lsp'], { stdio: ['pipe', 'pipe', 'inherit'] });
const closed = new Promise(resolve => child.on('close', resolve));
const hardTimeout = setTimeout(() => child.kill('SIGKILL'), 30000);
let buffer = Buffer.alloc(0), nextId = 1;
const pending = new Map(), notifications = [];
function send(method, params, id) {
  const body = JSON.stringify({ jsonrpc: '2.0', method, params, ...(id ? { id } : {}) });
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}
function request(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`timeout: ${method}`)), 5000);
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
    else notifications.push(message);
  }
});
async function waitFor(fn, ready) {
  for (let attempt = 0; attempt < 200; ++attempt) {
    const result = await fn();
    if (ready(result)) return result;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error('expected current result did not arrive');
}
const completion = () => request('textDocument/completion', { textDocument: { uri }, position: { line: 2, character: 'import provider('.length } });
const open = (uri, text, version = 1) => send('textDocument/didOpen', { textDocument: { uri, languageId: 'kio', version, text } });
const close = uri => send('textDocument/didClose', { textDocument: { uri } });
const change = (text, version) => send('textDocument/didChange', { textDocument: { uri: providerUri, version }, contentChanges: [{ text }] });
function expectOnly(result, name) {
  assert.equal(result.isIncomplete, false);
  assert.deepEqual(result.items.map(item => item.label), [name]);
  assert.ok(result.items[0].detail.includes(`fn ${name}`));
  assert.ok(JSON.stringify(result.items[0].documentation).includes('Current declaration'));
}
try {
  await request('initialize', { processId: process.pid, rootUri: pathToFileURL(resolve('.')).href, capabilities: {} });
  send('initialized', {});
  open(uri, source);
  await waitFor(async () => notifications, notes => notes.some(message => message.method === 'textDocument/publishDiagnostics' && message.params.uri === providerUri && message.params.diagnostics.some(d => d.severity === 1)));
  expectOnly(await completion(), 'chosen');
  const alternate = provider.replace('chosen', 'alternate');
  open(providerUri, alternate);
  expectOnly(await completion(), 'alternate');
  const aliasUri = providerUri.replace('provider.kio', '%70rovider.kio');
  open(aliasUri, provider);
  const ambiguous = await completion();
  assert.equal(ambiguous.isIncomplete, true);
  assert.deepEqual(ambiguous.items, []);
  close(aliasUri);
  expectOnly(await completion(), 'alternate');
  change(alternate.replace('module provider;', 'module wrong;'), 2);
  const invalid = await completion();
  assert.equal(invalid.isIncomplete, true);
  assert.deepEqual(invalid.items, []);
  change(alternate, 3);
  expectOnly(await waitFor(completion, result => result.items.some(item => item.label === 'alternate')), 'alternate');
  for (const [visibility, version] of [['pub ', 4], ['', 5]]) {
    change(`${alternate}\n${visibility}fn alternate(value: .) { value }`, version);
    const duplicate = await completion();
    assert.equal(duplicate.isIncomplete, true);
    assert.deepEqual(duplicate.items, []);
  }
  for (const [visibility, version] of [['pub ', 6], ['', 7]]) {
    change(`module provider; pub labels { field: . }; pub labels { other: . }; ${visibility}type {field} = {other};`, version);
    const duplicate = await completion();
    assert.equal(duplicate.isIncomplete, true);
    assert.deepEqual(duplicate.items, []);
  }
  change(alternate, 8);
  expectOnly(await completion(), 'alternate');
  close(providerUri);
  expectOnly(await completion(), 'chosen');
  unlinkSync('provider.kio');
  const removed = await completion();
  assert.equal(removed.isIncomplete, true);
  assert.deepEqual(removed.items, []);
  // The suffix alone is valid, but this file cannot declare `provider`
  // relative to the owning package root.
  mkdirSync('nested');
  writeFileSync('nested/provider.kio', provider);
  const nestedUri = pathToFileURL(resolve('nested/provider.kio')).href;
  send('textDocument/didChange', { textDocument: { uri, version: 2 }, contentChanges: [{ text: source }] });
  await waitFor(async () => notifications, notes => notes.some(message => message.method === 'textDocument/publishDiagnostics' && message.params.uri === nestedUri && message.params.diagnostics.some(d => d.severity === 1)));
  const misplaced = await completion();
  assert.equal(misplaced.isIncomplete, true);
  assert.deepEqual(misplaced.items, []);
  await request('shutdown', null); send('exit', null); child.stdin.end();
  assert.equal(await closed, 0);
  console.log('completion: current declaration snapshots survive body errors');
} finally {
  clearTimeout(hardTimeout);
  if (child.exitCode === null) { child.kill('SIGTERM'); await closed; }
}
JS
