#!/bin/sh
# Public completion keeps lexical visibility and the shadowing declaration's
# own metadata. JS is only the routing target for this frontend-only case;
# no backend output is consumed. The client waits for typed completion data.
set -eu

cd workdir
"$KIO_BIN" test >/dev/null
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const source = readFileSync('pkg/main.kio', 'utf8');
const uri = pathToFileURL(resolve('pkg/main.kio')).href;
const lines = source.split('\n');
const line = lines.findIndex(text => text.startsWith('fn consumer('));
assert.ok(line >= 0, 'completion site');
const position = { line, character: lines[line].lastIndexOf('value') + 1 };
const child = spawn(process.env.KIO_BIN, ['lsp'], {
  stdio: ['pipe', 'pipe', 'inherit'],
});
let buffer = Buffer.alloc(0);
let stage = 'initialize';
let retry;
let failed = false;
let hardTimeout;
function fail(error) {
  if (failed) return;
  failed = true;
  console.error(String(error));
  process.exitCode = 1;
  clearTimeout(retry);
  child.stdin.end();
  child.kill('SIGTERM');
  hardTimeout = setTimeout(() => child.kill('SIGKILL'), 2000);
}
const timeout = setTimeout(() => fail('LSP completion timed out'), 30000);
function send(method, params, id) {
  const body = JSON.stringify({ jsonrpc: '2.0', method, params, id });
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}
function complete() {
  send('textDocument/completion', { textDocument: { uri }, position }, 2);
}
function receive(message) {
  if (message.id === undefined || failed) return;
  assert.equal(message.error, undefined, JSON.stringify(message));
  if (stage === 'initialize') {
    assert.equal(message.id, 1);
    assert.ok(message.result.capabilities.completionProvider);
    send('initialized', {});
    send('textDocument/didOpen', {
      textDocument: { uri, languageId: 'kio', version: 1, text: source },
    });
    stage = 'completion';
    complete();
  } else if (stage === 'completion') {
    assert.equal(message.id, 2);
    const items = message.result.items;
    const local = items.find(item => item.label === 'value');
    assert.ok(local, JSON.stringify(items));
    if (local.detail === undefined) {
      retry = setTimeout(complete, 50);
      return;
    }
    assert.equal(message.result.isIncomplete, false);
    assert.equal(items.filter(item => item.label === 'value').length, 1);
    assert.equal(local.kind, 6, JSON.stringify(local));
    assert.equal(local.detail, '.', JSON.stringify(local));
    assert.equal(local.documentation, undefined, JSON.stringify(local));
    assert.ok(!items.some(item => ['consumer', 'later'].includes(item.label)),
      JSON.stringify(items));
    stage = 'shutdown';
    send('shutdown', null, 3);
  } else {
    assert.equal(stage, 'shutdown');
    assert.equal(message.id, 3);
    stage = 'exit';
    send('exit', null);
    child.stdin.end();
  }
}
child.stdout.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  try {
    for (;;) {
      const headerEnd = buffer.indexOf('\r\n\r\n');
      if (headerEnd < 0) break;
      const header = buffer.subarray(0, headerEnd).toString();
      const match = /^Content-Length: (\d+)$/mi.exec(header);
      assert.ok(match, header);
      const end = headerEnd + 4 + Number(match[1]);
      if (buffer.length < end) break;
      const message = JSON.parse(buffer.subarray(headerEnd + 4, end).toString());
      buffer = buffer.subarray(end);
      receive(message);
    }
  } catch (error) { fail(error); }
});
child.on('error', fail);
child.stdin.on('error', fail);
child.on('close', code => {
  clearTimeout(timeout);
  clearTimeout(hardTimeout);
  clearTimeout(retry);
  if (failed) return;
  if (code !== 0 || stage !== 'exit') {
    console.error(`LSP exited ${code} during ${stage}`);
    process.exitCode = 1;
  } else {
    console.log('completion: exact scope and shadow identity');
  }
});
send('initialize', {
  processId: process.pid,
  rootUri: pathToFileURL(resolve('.')).href,
  capabilities: {},
}, 1);
JS
