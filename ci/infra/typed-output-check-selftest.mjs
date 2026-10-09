import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const repo = process.argv[2];
// Optional consumer directory/control select the same witness against old scripts.
const consumerDir = process.argv[3] ?? path.join(repo, 'ci/checks/per-case');
const selectedControl = process.argv[4];
const selectedBackend = process.argv[5];
const fixture = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'typed-output-selftest.'));
const bin = path.join(fixture, 'bin');
const events = path.join(fixture, 'events');
let controls = 0;

try {
  fs.mkdirSync(bin);
  const mock = path.join(fixture, 'mock.mjs');
  fs.writeFileSync(mock, `
import fs from 'node:fs';
import path from 'node:path';
const [tool, ...args] = process.argv.slice(2);
if (tool === 'mise') {
  if (args[0] !== 'which') process.exit(98);
  console.log(args[1] === 'node' ? process.execPath : path.join(process.env.TYPED_TEST_BIN, args[1]));
} else if (tool === 'kio') {
  const config = JSON.parse(fs.readFileSync('fixture.json', 'utf8'));
  if (args[0] === 'debug' && args[1] === 'build-output-plan') {
    if (config.queryFailure) process.exit(11);
    console.log(JSON.stringify(config.rows.map(row => ({
      package_root: process.cwd() + '/' + (row.package ?? '.'),
      target: args[2], backend: row.backend,
      output_dir: row.absolute ?? process.cwd() + '/' + row.output,
      namespace: row.namespace
    }))));
  } else if (args[0] === 'build') {
    fs.appendFileSync(process.env.TYPED_TEST_EVENTS, JSON.stringify({ build: true }) + '\\n');
    if (config.buildFailure) process.exit(14);
    if (config.noop) process.exit(0);
    for (const row of config.rows) {
      if (row.backend !== args[1]) continue;
      const out = row.absolute ?? process.cwd() + '/' + row.output;
      fs.mkdirSync(out, { recursive: true });
      if (row.missing) continue;
      const runtime = path.join(out, row.namespace + (row.backend === 'ts' ? '.js' : '.py'));
      fs.writeFileSync(runtime, 'runtime');
      if (row.backend === 'ts') fs.writeFileSync(path.join(out, row.namespace + '.d.ts'), row.invalid ? 'invalid' : 'valid');
      else {
        const stub = path.join(out, row.namespace);
        fs.mkdirSync(stub, { recursive: true });
        fs.writeFileSync(path.join(stub, '__init__.pyi'), 'from ._kio_stub_0000 import *\\n');
        fs.writeFileSync(path.join(stub, '_kio_stub_0000.pyi'), row.invalid ? 'invalid' : 'valid');
      }
    }
  } else process.exit(98);
} else {
  if (tool === 'tsc' && (args.length !== 3 || args[0] !== '--strict' || args[1] !== '--noEmit')) process.exit(98);
  if (tool === 'pyright') {
    if (args.length !== 3 || args[0] !== '--project') process.exit(98);
    const policy = JSON.parse(fs.readFileSync(args[1], 'utf8'));
    if (policy.typeCheckingMode !== 'strict' || policy.pythonVersion !== '3.10') process.exit(98);
  }
  const input = args.at(-1);
  fs.appendFileSync(process.env.TYPED_TEST_EVENTS, JSON.stringify({ input }) + '\\n');
  const text = fs.readFileSync(tool === 'tsc' ? input : path.join(input, '_kio_stub_0000.pyi'), 'utf8');
  if (text === 'invalid') process.exit(1);
}
`);
  for (const tool of ['kio', 'mise', 'tsc', 'pyright']) {
    fs.writeFileSync(path.join(bin, tool), `#!/bin/sh\nexec "$TYPED_TEST_NODE" "$TYPED_TEST_MOCK" ${tool} "$@"\n`, { mode: 0o755 });
  }
  const env = {
    ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`,
    KIO_BIN: path.join(bin, 'kio'), TYPED_TEST_BIN: bin, TYPED_TEST_NODE: process.execPath,
    TYPED_TEST_MOCK: mock, TYPED_TEST_EVENTS: events,
  };
  function run(backend, name, config, expected, checkerCalls, setup = () => {}) {
    if (selectedControl && selectedControl !== name) return [];
    const caseDir = path.join(fixture, `${backend}-${name}`);
    const workdir = path.join(caseDir, 'workdir');
    fs.mkdirSync(workdir, { recursive: true });
    fs.writeFileSync(path.join(workdir, 'fixture.json'), JSON.stringify(config));
    setup(workdir);
    fs.writeFileSync(events, '');
    const script = backend === 'ts' ? 'tsc-strict.sh' : 'pyright-strict.sh';
    const result = spawnSync('sh', [path.join(consumerDir, script)], {
      cwd: caseDir, env: { ...env, KIO_TARGET: backend }, encoding: 'utf8',
    });
    assert.equal(result.status, expected, `${backend}/${name}: ${result.stderr}`);
    const calls = fs.readFileSync(events, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
    assert.equal(calls.filter(call => call.input).length, checkerCalls, `${backend}/${name}: checker invocation count`);
    controls++;
    return calls;
  }
  for (const backend of ['ts', 'python']) {
    if (selectedBackend && selectedBackend !== backend) continue;
    const rows = [
      { backend, package: 'pkg a', output: 'pkg a/custom output', namespace: backend === 'python' ? 'class_' : 'alpha' },
      { backend, package: 'pkg_b', output: 'pkg_b/../pkg_b/skin\nquoted\'"', namespace: 'beta' },
    ];
    const calls = run(backend, 'nested', { rows }, 0, 2);
    if (!selectedControl || selectedControl === 'nested') {
      assert.ok(calls.some(call => call.input?.includes('skin\nquoted\'"')), 'output argument lost control characters');
    }
    run(backend, 'invalid-second', { rows: [rows[0], { ...rows[1], invalid: true }] }, 1, 2);
    run(backend, 'missing-second', { rows: [rows[0], { ...rows[1], missing: true }] }, 1, 1);
    run(backend, 'zero-plan', { rows: [] }, 1, 0);
    run(backend, 'query-failure', { rows, queryFailure: true }, 1, 0);
    run(backend, 'query-and-build-failure', { rows, queryFailure: true, buildFailure: true }, 0, 0);
    run(backend, 'build-failure', { rows, buildFailure: true }, 0, 0);
    run(backend, 'actual-backend', { rows: [{ ...rows[0], backend: 'js', namespace: null }] }, 0, 0);
    run(backend, 'metadata-name', { rows: [{ ...rows[0], output: 'typed-output-plan.json' }] }, 0, 1);
    run(backend, 'config-name', { rows: [{ ...rows[0], output: 'pyrightconfig.json' }] }, 0, 1);
    if (backend === 'python') {
      run(backend, 'nested-config', { rows }, 0, 2, workdir => {
        fs.mkdirSync(path.join(workdir, 'pkg a'));
        fs.writeFileSync(path.join(workdir, 'pkg a', 'pyrightconfig.json'), '{ "typeCheckingMode": "off" }');
      });
    }
    run(backend, 'stale-noop', { rows: [rows[0]], noop: true }, 1, 0, workdir => {
      const out = path.join(workdir, rows[0].output);
      fs.mkdirSync(out, { recursive: true });
      fs.writeFileSync(path.join(out, `${rows[0].namespace}.${backend === 'ts' ? 'js' : 'py'}`), 'runtime');
      if (backend === 'ts') fs.writeFileSync(path.join(out, `${rows[0].namespace}.d.ts`), 'valid');
      else {
        const stub = path.join(out, rows[0].namespace);
        fs.mkdirSync(stub);
        fs.writeFileSync(path.join(stub, '__init__.pyi'), 'from ._kio_stub_0000 import *\n');
        fs.writeFileSync(path.join(stub, '_kio_stub_0000.pyi'), 'valid');
      }
    });
    run(backend, 'leaf-symlink', { rows: [{ ...rows[0], output: 'linked-output/' }] }, 0, 1, workdir => {
      fs.mkdirSync(path.join(workdir, 'old-output'));
      fs.writeFileSync(path.join(workdir, 'old-output', 'sentinel'), 'keep');
      fs.symlinkSync('old-output', path.join(workdir, 'linked-output'));
    });
    const outside = path.join(fixture, `${backend}-outside`);
    fs.mkdirSync(outside);
    const sentinel = path.join(outside, 'sentinel');
    fs.writeFileSync(sentinel, 'keep');
    run(backend, 'outside', { rows: [rows[0], { ...rows[1], absolute: outside }] }, 1, 0);
    assert.equal(fs.readFileSync(sentinel, 'utf8'), 'keep');
    for (const [name, output] of [['outside-leaf', 'link'], ['outside-parent', 'link/child']]) {
      run(backend, name, { rows: [{ ...rows[0], output }] }, 1, 0, workdir => fs.symlinkSync(outside, path.join(workdir, 'link')));
      assert.equal(fs.readFileSync(sentinel, 'utf8'), 'keep');
    }
  }

  if (!selectedControl) {
    // A later invalid row must prevent deletion of an earlier valid row.
    const owned = path.join(fixture, 'owned');
    const safeOutput = path.join(owned, 'out');
    fs.mkdirSync(safeOutput, { recursive: true });
    fs.writeFileSync(path.join(safeOutput, 'sentinel'), 'keep');
    const inventory = path.join(fixture, 'inventory.json');
    const row = { package_root: owned, target: 'ts', backend: 'ts', output_dir: safeOutput, namespace: 'alpha' };
    fs.writeFileSync(inventory, JSON.stringify([row, { ...row, output_dir: fixture }]));
    const validation = spawnSync(process.execPath, [path.join(repo, 'ci/infra/typed-output-check.mjs'), 'prepare', owned, inventory, 'ts']);
    assert.equal(validation.status, 1);
    assert.equal(fs.readFileSync(path.join(safeOutput, 'sentinel'), 'utf8'), 'keep');
    controls++;

    const referent = path.join(owned, 'referent');
    fs.mkdirSync(referent);
    fs.writeFileSync(path.join(referent, 'sentinel'), 'keep');
    const leaf = path.join(owned, 'leaf');
    fs.symlinkSync('referent', leaf);
    fs.writeFileSync(inventory, JSON.stringify([{ ...row, output_dir: `${leaf}/` }]));
    const unlink = spawnSync(process.execPath, [path.join(repo, 'ci/infra/typed-output-check.mjs'), 'prepare', owned, inventory, 'ts']);
    assert.equal(unlink.status, 0, unlink.stderr?.toString());
    assert.equal(fs.existsSync(leaf), false);
    assert.equal(fs.readFileSync(path.join(referent, 'sentinel'), 'utf8'), 'keep');
    controls++;

    const actual = path.join(owned, 'real', 'out');
    fs.mkdirSync(path.join(owned, 'real', 'deeper'), { recursive: true });
    fs.mkdirSync(actual);
    fs.symlinkSync('real/deeper', path.join(owned, 'link'));
    fs.writeFileSync(inventory, JSON.stringify([{ ...row, output_dir: `${owned}/link/../out` }]));
    const componentPath = spawnSync(process.execPath, [path.join(repo, 'ci/infra/typed-output-check.mjs'), 'prepare', owned, inventory, 'ts']);
    assert.equal(componentPath.status, 0, componentPath.stderr?.toString());
    assert.equal(fs.existsSync(actual), false, 'cleanup changed the access path meaning');
    assert.equal(fs.readFileSync(path.join(safeOutput, 'sentinel'), 'utf8'), 'keep');
    controls++;
    fs.mkdirSync(actual);
    fs.writeFileSync(path.join(actual, 'alpha.js'), 'runtime');
    fs.writeFileSync(path.join(actual, 'alpha.d.ts'), 'valid');
    fs.writeFileSync(events, '');
    const checked = spawnSync(process.execPath, [path.join(repo, 'ci/infra/typed-output-check.mjs'), 'check', owned, inventory, 'ts', path.join(bin, 'tsc'), '--strict', '--noEmit'], { env });
    assert.equal(checked.status, 0, checked.stderr?.toString());
    const checkedInput = JSON.parse(fs.readFileSync(events, 'utf8').trim()).input;
    assert.equal(checkedInput, path.join(actual, 'alpha.d.ts'), 'checker received a different physical output');
    controls++;
  }
  console.log(`typed-output-check selftest: ${controls} controls passed`);
} finally {
  fs.rmSync(fixture, { recursive: true, force: true });
}
