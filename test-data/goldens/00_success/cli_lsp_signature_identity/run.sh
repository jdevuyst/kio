#!/bin/sh
# Public JSON-RPC responses identify the resolved declaration, including a
# qualified host function and same-spelled functions in distinct providers.
set -eu
cd workdir
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const source = readFileSync('main.kio', 'utf8');
const uri = pathToFileURL(resolve('main.kio')).href;
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
    pending.set(id, message => {
      clearTimeout(timeout);
      message.error ? reject(message.error) : resolve(message.result);
    });
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
function position(name) {
  const lines = source.split('\n');
  const line = lines.findIndex(text => text.startsWith(`pub fn ${name}(`));
  assert.ok(line >= 0);
  return { textDocument: { uri }, position: { line, character: lines[line].lastIndexOf('()') + 1 } };
}
try {
  await request('initialize', { processId: process.pid, rootUri: pathToFileURL(resolve('.')).href, capabilities: {} });
  send('initialized', {});
  send('textDocument/didOpen', { textDocument: { uri, languageId: 'kio', version: 1, text: source } });
  let ready = null;
  for (let attempt = 0; attempt < 200 && ready === null; ++attempt) {
    ready = await request('textDocument/signatureHelp', position('bare'));
    if (ready === null) await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.notEqual(ready, null, 'clean typed snapshot must become ready');
  const failures = [];
  for (const [name, label] of [
    ['bare', 'fn choose[A](left: A, right: A) -> A'],
    ['qualified', 'fn choose[A](left: A, right: A) -> A'],
    ['distinct', 'fn choose(first: ., second: .) -> .'],
    ['host', 'host fn supply(left: ., right: .) -> .'],
    ['typed', 'fn choose[A](left: A, right: A) -> A'],
  ]) {
    const result = await request('textDocument/signatureHelp', position(name));
    try {
      assert.notEqual(result, null, name);
      const signature = result.signatures[0];
      assert.equal(signature.label, label, name);
      assert.equal(result.activeParameter, 1, name);
      assert.equal(signature.activeParameter, 1, name);
    } catch (error) { failures.push(String(error)); }
  }
  assert.equal(await request('textDocument/signatureHelp', position('local')), null);
  assert.deepEqual(failures, []);
  await request('shutdown', null);
  send('exit', null);
  child.stdin.end();
  assert.equal(await closed, 0);
  console.log('signature help: qualified callable identity');
} finally {
  clearTimeout(hardTimeout);
  if (child.exitCode === null) { child.kill('SIGTERM'); await closed; }
}
JS
