#!/bin/sh
# These requests exercise syntax-backed identity, independent of typed readiness.
set -eu
cd workdir
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

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
async function completion(uri, marked) {
  const cursor = marked.indexOf('|');
  assert.ok(cursor >= 0);
  const text = marked.replace('|', '');
  const prefix = text.slice(0, cursor).split('\n');
  send('textDocument/didOpen', { textDocument: { uri, languageId: 'kio', version: 1, text } });
  const result = await request('textDocument/completion', {
    textDocument: { uri }, position: { line: prefix.length - 1, character: prefix.at(-1).length },
  });
  send('textDocument/didClose', { textDocument: { uri } });
  return result;
}
const failures = [];
function check(name, action) {
  try { action(); } catch (error) { failures.push(`${name}: ${error}`); }
}
try {
  await request('initialize', { processId: process.pid, rootUri: pathToFileURL(resolve('.')).href, capabilities: {} });
  send('initialized', {});
  const cases = [
    ['.pkg', 'package probe;', 'build { target js { baseline |latest } }', 'build { target js { baseline |', ['()', 'es2020', 'latest']],
    ['.dep', 'dependency probe;', 'source { |path "vendor.pkg.kio"; }', 'source { |', ['git', 'path', 'ref']],
    ['.lock', 'lock probe;', 'resolved { |git "url"; ref "main"; commit "sha"; sig "digest"; }', 'resolved { |', ['commit', 'git', 'path', 'ref', 'sig']],
    ['.sig', 'signature probe v(1);', '|v(1) { nonbreaking { add { module main { pub type A = .; } } } }', 'v(1) { |', ['breaking', 'nonbreaking', 'with']],
    ['', 'module probe;', '|fn run() { () }', 'fn run() { |', null],
  ];
  for (const [kind, header, completeBody, incompleteBody, expected] of cases) {
    const matching = pathToFileURL(resolve(`probe${kind}.kio`)).href;
    const wrongPath = resolve(`wrong${kind}.kio`);
    assert.equal(existsSync(wrongPath), false, 'mismatched overlays need no disk file');
    const wrong = pathToFileURL(wrongPath).href;
    for (const [context, marked] of [
      ['complete body', `${header}\n${completeBody}`],
      ['incomplete body', `${header}\n${incompleteBody}`],
      ['malformed body', `${header}\n${incompleteBody}@`],
      ['comment', `${header}\n// co|mment`],
      ['doc comment', `${header}\n/// do|cumentation`],
      ['header binder', header.replace('probe', 'pro|be')],
    ]) {
      const coherent = await completion(matching, marked);
      check(`${kind} ${context} coherent`, () => assert.notEqual(coherent, null));
      if (context === 'incomplete body' && expected !== null) {
        check(`${kind} incomplete body vocabulary`, () => {
          assert.deepEqual(coherent.items.map(item => item.label).sort(), expected);
          assert.equal(coherent.isIncomplete, false);
        });
      }
      const mismatch = await completion(wrong, marked);
      check(`${kind} ${context} mismatch`, () => assert.equal(mismatch, null));
      const nonfile = await completion(`untitled:wrong${kind}.kio`, marked);
      check(`${kind} ${context} nonfile`, () => assert.notEqual(nonfile, null));
    }
    const prefix = header.slice(0, -1);
    const incompleteHeader = await completion(wrong, `${prefix}|`);
    check(`${kind} incomplete header`, () => assert.notEqual(incompleteHeader, null));
    const keyword = header.split(' ')[0];
    const initial = await completion(wrong, `${keyword.slice(0, -1)}|`);
    check(`${kind} header keyword prefix`, () => assert.deepEqual(initial.items.map(item => item.label), [keyword]));
    const missingPath = resolve(`absent/probe${kind}.kio`);
    assert.equal(existsSync(missingPath), false);
    const absent = await completion(pathToFileURL(missingPath).href, `${header}\n${incompleteBody}`);
    check(`${kind} coherent missing file`, () => assert.notEqual(absent, null));
    const invalid = await completion(`file:relative${kind}.kio`, `${header}\n${incompleteBody}`);
    check(`${kind} invalid file URI`, () => assert.equal(invalid, null));
  }
  assert.deepEqual(failures, []);
  await request('shutdown', null);
  send('exit', null);
  child.stdin.end();
  assert.equal(await closed, 0);
  console.log('completion: completed file headers authenticate identity');
} finally {
  if (child.exitCode === null) { child.kill('SIGTERM'); await closed; }
  clearTimeout(hardTimeout);
}
JS
