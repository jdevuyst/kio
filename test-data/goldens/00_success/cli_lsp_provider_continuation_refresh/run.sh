#!/bin/sh
# JS is only the routing target for this frontend-only public LSP case;
# no generated backend output is consumed.
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
const provider = readFileSync('pkg/provider.kio', 'utf8');
const changed = provider.replace('product fallback;', 'product alternate;');
assert.notEqual(changed, provider);
const uri = pathToFileURL(resolve('pkg/main.kio')).href;
const providerUri = pathToFileURL(resolve('pkg/provider.kio')).href;
const prefix = source.slice(0, source.indexOf('fallback'));
assert.ok(prefix.includes('choose!'));
const prefixLines = prefix.split('\n');
const position = { line: prefixLines.length - 1, character: prefixLines.at(-1).length };
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
const timeout = setTimeout(() => fail(`LSP completion timed out during ${stage}`), 15000);
function send(method, params, id) {
  const body = JSON.stringify({ jsonrpc: '2.0', method, params, id });
  child.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}
function edit(documentUri, version, text) {
  send('textDocument/didChange', {
    textDocument: { uri: documentUri, version }, contentChanges: [{ text }],
  });
}
function complete() {
  send('textDocument/completion', { textDocument: { uri }, position }, 2);
}
function offset(text, point) {
  const lines = text.split('\n');
  assert.ok(point.line < lines.length && point.character <= lines[point.line].length);
  return lines.slice(0, point.line).reduce((sum, line) => sum + line.length + 1, 0)
    + point.character;
}
function receive(message) {
  if (failed) return;
  if (message.id === undefined) {
    if (stage === 'incomplete' && message.method === 'textDocument/publishDiagnostics'
        && message.params.uri === uri && message.params.version === 2) {
      assert.ok(message.params.diagnostics.length > 0);
      stage = 'before';
      complete();
    }
    if (stage === 'analysis' && message.method === 'textDocument/publishDiagnostics'
        && message.params.uri === uri && message.params.version === 3) {
      assert.deepEqual(message.params.diagnostics.map(diagnostic => ({
        message: diagnostic.message,
        severity: diagnostic.severity,
        source: diagnostic.source,
        range: diagnostic.range,
        tags: diagnostic.tags,
        version: diagnostic.data?.kioDocumentVersion,
      })), [{
        message: 'private function `run` is transitively unused',
        severity: 2,
        source: 'kio',
        range: {
          start: { line: 4, character: 3 },
          end: { line: 4, character: 6 },
        },
        tags: [1],
        version: 3,
      }]);
      assert.equal(readFileSync('pkg/provider.kio', 'utf8'), provider);
      stage = 'shutdown';
      send('shutdown', null, 3);
    }
    return;
  }
  assert.equal(message.error, undefined, JSON.stringify(message));
  if (stage === 'initialize') {
    assert.equal(message.id, 1);
    assert.ok(message.result.capabilities.completionProvider);
    send('initialized', {});
    for (const [documentUri, text] of [[providerUri, provider], [uri, source]]) {
      send('textDocument/didOpen', {
        textDocument: { uri: documentUri, languageId: 'kio', version: 1, text },
      });
    }
    stage = 'initial';
    complete();
  } else if (['initial', 'before', 'after'].includes(stage)) {
    assert.equal(message.id, 2);
    const labels = message.result.items.map(item => item.label);
    const expected = stage === 'after' ? 'alternate' : 'fallback';
    assert.ok(labels.length === 0 || (labels.length === 1 && labels[0] === expected),
      `${stage}: ${JSON.stringify(message.result)}`);
    if (labels.length === 0) {
      retry = setTimeout(complete, 50);
      return;
    }
    if (stage === 'initial') {
      stage = 'incomplete';
      edit(uri, 2, prefix);
    } else if (stage === 'before') {
      stage = 'after';
      edit(providerUri, 2, changed);
      complete();
    } else {
      const actual = message.result.items[0].textEdit;
      assert.equal(actual.newText, 'alternate');
      const applied = prefix.slice(0, offset(prefix, actual.range.start))
        + actual.newText + prefix.slice(offset(prefix, actual.range.end));
      assert.equal(applied, `${prefix}alternate`);
      assert.equal(readFileSync('pkg/provider.kio', 'utf8'), provider);
      stage = 'analysis';
      edit(uri, 3, `${applied} { () }\n}\n`);
    }
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
    console.log('completion: unsaved provider refresh applies and reanalyses');
  }
});
send('initialize', {
  processId: process.pid,
  rootUri: pathToFileURL(resolve('.')).href,
  capabilities: {},
}, 1);
JS
