#!/bin/sh
# Shared REPL diagnostics and retained session state; no backend is consumed.
# Module syntax is Kio', but the REPL command belongs to the full compiler.
set -eu
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

assert.ok(process.env.TMPDIR, 'TMPDIR is required');
const scratch = mkdtempSync(join(process.env.TMPDIR, 'kio-reload-diagnostic-'));
cpSync('workdir', scratch, { recursive: true });
const sourcePath = join(scratch, 'demo.kio');
const source = readFileSync(sourcePath, 'utf8');
const child = spawn(process.env.KIO_BIN, ['repl', 'demo'], {
  cwd: scratch,
  env: { ...process.env, NO_COLOR: '1', TERM: 'dumb' },
  stdio: ['pipe', 'pipe', 'pipe'],
});
let stdout = '';
let stderr = '';
let stage = 'load';
let outstanding = false;
let timedOut = false;
const deadline = setTimeout(() => {
  timedOut = true;
  child.kill('SIGKILL');
}, 30000);

function poll() {
  if (stage === 'reload' && !outstanding) {
    outstanding = true;
    child.stdin.write(':mods\n');
  }
}
child.stdout.on('data', chunk => {
  stdout += chunk;
  if (stage === 'load' && stdout.includes('loaded: demo\n')) {
    const previous = statSync(sourcePath);
    writeFileSync(sourcePath, source.replace('{ helper() }', '{ helpr() }'));
    assert.notEqual(readFileSync(sourcePath, 'utf8'), source);
    utimesSync(sourcePath, previous.atime, new Date(previous.mtimeMs + 2000));
    stage = 'reload';
    poll();
  } else if (stage === 'reload' && stdout.endsWith('* demo\n')) {
    outstanding = false;
    setImmediate(poll);
  }
});
child.stderr.on('data', chunk => {
  stderr += chunk;
  if (stage === 'reload' && stderr.includes('reload failed')) {
    stage = 'retained';
    child.stdin.end(':source run\n:quit\n');
  }
});

try {
  const code = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', resolve);
  });
  assert.equal(timedOut, false, `REPL did not finish:\n${stdout}\n${stderr}`);
  assert.equal(code, 0, stderr);
  assert.equal(stage, 'retained');
  assert.match(stderr, /reload failed.*keeping the previous version/);
  assert.match(stderr, /\|.*helpr\(\)/);
  assert.match(stderr, /\|\s+\^+/);
  assert.match(stderr, /help:.*helper/);
  assert.match(stdout, /pub fn run\(\)[\s\S]*helper\(\)/);
  assert.doesNotMatch(stdout, /helpr\(\)/);
  console.log('reload diagnostic; previous session retained');
} finally {
  clearTimeout(deadline);
  child.kill();
  rmSync(scratch, { recursive: true, force: true });
}
JS
