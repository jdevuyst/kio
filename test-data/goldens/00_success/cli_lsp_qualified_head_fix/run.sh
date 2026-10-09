#!/bin/sh
# Public LSP edits own only the misspelled qualifier, not its member suffix.
# The clean disk fixture still runs ordinary package checks and invariants.
# JS only routes this compiler-tool test; no backend artifact is consumed.
set -eu
cd workdir
"$KIO_BIN" test >/dev/null
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const clean = readFileSync('main.kio', 'utf8');
assert.ok(clean.includes('support.helper'));
const source = clean.replace('support.helper', 'suport. // 😀 keep member\n  helper');
const uri = pathToFileURL(resolve('main.kio')).href;
const lines = source.split('\n');
const line = lines.findIndex(text => text.includes('suport.'));
const start = { line, character: lines[line].indexOf('suport.') };
const end = { line, character: start.character + 'suport'.length };
const child = spawn(process.env.KIO_BIN, ['lsp'], {
  stdio: ['pipe', 'pipe', 'inherit'],
});
let buffer = Buffer.alloc(0);
let stage = 'initialize';
let failed = false;
let hardTimeout;
function fail(error) {
  if (failed) return;
  failed = true;
  console.error(String(error));
  process.exitCode = 1;
  child.stdin.end();
  child.kill('SIGTERM');
  hardTimeout = setTimeout(() => child.kill('SIGKILL'), 2000);
}
const timeout = setTimeout(() => fail('LSP quick fix timed out'), 30000);
function send(method, params, id) {
  const body = JSON.stringify({ jsonrpc: '2.0', method, params, id });
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}
function receive(message) {
  if (failed) return;
  assert.equal(message.error, undefined, JSON.stringify(message));
  if (stage === 'initialize' && message.id === 1) {
    assert.ok(message.result.capabilities.codeActionProvider);
    stage = 'diagnostic';
    send('initialized', {});
    send('textDocument/didOpen', {
      textDocument: { uri, languageId: 'kio', version: 1, text: source },
    });
  } else if (stage === 'diagnostic'
      && message.method === 'textDocument/publishDiagnostics'
      && message.params.uri === uri && message.params.version === 1) {
    const diagnostics = message.params.diagnostics;
    assert.equal(diagnostics.length, 1, JSON.stringify(message));
    assert.equal(diagnostics[0].code, 13);
    assert.match(diagnostics[0].message, /unbound name/);
    stage = 'action';
    send('textDocument/codeAction', {
      textDocument: { uri }, range: diagnostics[0].range,
      context: { diagnostics, only: ['quickfix'] },
    }, 2);
  } else if (stage === 'action' && message.id === 2) {
    const actions = message.result.filter(action => action.edit !== undefined);
    assert.equal(actions.length, 1, JSON.stringify(message));
    assert.equal(actions[0].isPreferred, true);
    assert.equal(actions[0].command, undefined);
    const documents = actions[0].edit.documentChanges;
    assert.equal(documents.length, 1);
    assert.deepEqual(documents[0].textDocument, { uri, version: 1 });
    const edits = documents[0].edits;
    assert.deepEqual(edits, [{ range: { start, end }, newText: 'support' }]);
    const offset = lines.slice(0, line).reduce((sum, text) => sum + text.length + 1, 0)
      + start.character;
    const repaired = source.slice(0, offset) + edits[0].newText
      + source.slice(offset + end.character - start.character);
    assert.equal(repaired, source.replace('suport.', 'support.'));
    stage = 'reanalysis';
    send('textDocument/didChange', {
      textDocument: { uri, version: 2 }, contentChanges: [{ text: repaired }],
    });
  } else if (stage === 'reanalysis'
      && message.method === 'textDocument/publishDiagnostics'
      && message.params.uri === uri && message.params.version === 2) {
    assert.deepEqual(message.params.diagnostics, []);
    stage = 'shutdown';
    send('shutdown', null, 3);
  } else if (stage === 'shutdown' && message.id === 3) {
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
  if (failed) return;
  if (code !== 0 || stage !== 'exit') {
    console.error(`LSP exited ${code} during ${stage}`);
    process.exitCode = 1;
  } else {
    console.log('quick fix: exact qualifier head and clean reanalysis');
  }
});
send('initialize', {
  processId: process.pid,
  rootUri: pathToFileURL(resolve('.')).href,
  capabilities: { workspace: { workspaceEdit: { documentChanges: true } } },
}, 1);
JS
