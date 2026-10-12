import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { generateDocsPages } from './prepare-docs.mjs'
import { docsReference } from './docs-reference.mjs'

const repoRoot = path.resolve(fileURLToPath(new URL('../..', import.meta.url)))
const helperUrl = new URL('./prepare-docs.mjs', import.meta.url).href
const posixBoundary = { skip: process.platform === 'win32' && 'fake executable shebang boundary requires POSIX' }

test('shared Reference sidebar retains current main links and order', () => {
  assert.deepEqual(docsReference(), { text: 'Reference', items: [
    { text: 'Language spec', link: 'https://github.com/jdevuyst/kio/blob/main/specs/language.md' },
    { text: 'CLI spec', link: 'https://github.com/jdevuyst/kio/blob/main/specs/cli.md' },
    { text: 'Package spec', link: 'https://github.com/jdevuyst/kio/blob/main/specs/package.md' },
    { text: 'Backend contracts', link: 'https://github.com/jdevuyst/kio/tree/main/specs/backends' }
  ] })
})

test('source metadata follows real docs and package inputs while synthetic catalogues have no source', t => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'website-source-pages.'))
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }))
  const rendered = path.join(scratch, 'rendered')
  fs.mkdirSync(rendered)
  fs.writeFileSync(path.join(rendered, 'README.html'), '<main>\n<h1>Docs</h1>\n</main>')
  fs.writeFileSync(path.join(rendered, 'package-boundary.html'), '<main>\n<h1>Package</h1>\n</main>')
  const sources = new Map()
  const sections = new Map([['guides', { route: 'guides', title: 'Guides', items: [] }]])
  generateDocsPages(sections, { output: path.join(scratch, 'output'), rendered,
    writePage: (page, title, body, source) => sources.set(page, source) })
  assert.equal(sources.get('index.md'), 'docs/README.md')
  assert.equal(sources.get('guides/index.md'), undefined)
  assert.equal(sources.get('package-boundary.md'), 'docs/docs.pkg.kio')
})

function invoke(t, expression, status = 0, entry = false) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'website-cargo-test.'))
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }))
  const record = path.join(scratch, 'invocation.json')
  for (const name of ['sh', 'cargo']) {
    fs.writeFileSync(path.join(scratch, name), `#!/usr/bin/env node
const fs = require('node:fs');
fs.writeFileSync(process.env.INVOCATION_RECORD, JSON.stringify({
  command: ${JSON.stringify(name)}, args: process.argv.slice(2), cwd: process.cwd()
}));
process.stdout.write('command output');
process.stderr.write('command diagnostic');
process.exit(Number(process.env.COMMAND_STATUS));
`, { mode: 0o755 })
  }
  if (status === null) fs.unlinkSync(path.join(scratch, 'sh'))
  const source = `import { runKiodocBuild, runKio } from ${JSON.stringify(helperUrl)}; ${expression}`
  let argv = ['--input-type=module', '-e', source]
  if (entry === 'stdin') argv = ['--input-type=module', '-']
  else if (entry === 'direct') argv = [fileURLToPath(helperUrl)]
  else if (entry) {
    const script = path.join(scratch, 'prepare-docs.mjs')
    fs.symlinkSync(fileURLToPath(helperUrl), script)
    argv = [script]
  }
  const result = spawnSync(process.execPath, argv, {
    cwd: scratch,
    input: entry === 'stdin' ? source : undefined,
    encoding: 'utf8',
    env: { ...process.env, PATH: status === null ? scratch : `${scratch}${path.delimiter}${process.env.PATH}`,
      INVOCATION_RECORD: record, COMMAND_STATUS: String(status) }
  })
  assert.equal(result.error, undefined)
  return { result, invocation: fs.existsSync(record) ? JSON.parse(fs.readFileSync(record, 'utf8')) : null }
}

test('Kiodoc build uses the Cargo wrapper with unchanged cwd, argv and inherited output', posixBoundary, t => {
  const { result, invocation } = invoke(t, 'runKiodocBuild()')
  assert.equal(result.status, 0)
  assert.equal(invocation.command, 'sh')
  assert.deepEqual(invocation.args, [path.join(repoRoot, 'ci', 'cargo.sh'),
    'run', '--quiet', '--manifest-path', '../kio-rs/Cargo.toml',
    '--bin', 'kio', '--', 'doc', 'build', '--html'])
  assert.equal(invocation.cwd, path.join(repoRoot, 'docs'))
  assert.equal(result.stdout, 'command output')
  assert.equal(result.stderr, 'command diagnostic')
})

test('Kio helper preserves spaced arguments and captured/default or inherited/custom output', posixBoundary, t => {
  const args = ['debug', 'tokens', 'a file with spaces.kio']
  for (const custom of [false, true]) {
    const options = custom ? { cwd: path.join(repoRoot, 'docs'), stdio: 'inherit' } : {}
    const { result, invocation } = invoke(t,
      `const output = runKio(${JSON.stringify(args)}, ${JSON.stringify(options)}); console.log(JSON.stringify(output))`)
    assert.equal(result.status, 0)
    assert.equal(invocation.command, 'sh')
    assert.deepEqual(invocation.args, [path.join(repoRoot, 'ci', 'cargo.sh'),
      'run', '--quiet', '--manifest-path', path.join(repoRoot, 'kio-rs', 'Cargo.toml'),
      '--bin', 'kio', '--', ...args])
    assert.equal(invocation.cwd, custom ? options.cwd : repoRoot)
    assert.equal(result.stdout, custom ? 'command outputnull\n' : '"command output"\n')
    assert.equal(result.stderr, custom ? 'command diagnostic' : '')
  }
})

test('nonzero Cargo status remains a failure with original helper diagnostics', posixBoundary, t => {
  for (const expression of ['runKiodocBuild()', "runKio(['check'])"]) {
    const { result } = invoke(t, expression, 17)
    assert.equal(result.status, 1)
    assert.equal(result.stdout, 'command output')
    assert.equal(result.stderr, expression === 'runKiodocBuild()'
      ? 'command diagnosticprepare-docs: kio doc build --html failed\n'
      : 'command diagnosticprepare-docs: kio check failed\n')
  }
})

test('missing shell remains a helper failure, and the CLI entry still executes preparation', posixBoundary, t => {
  for (const expression of ['runKiodocBuild()', "runKio(['check'])"]) {
    const { result, invocation } = invoke(t, expression, null)
    assert.equal(invocation, null)
    assert.equal(result.status, 1)
    assert.equal(result.stdout, '')
    assert.match(result.stderr, /^prepare-docs: kio (?:doc build --html|check) failed\n$/)
  }
  const inert = invoke(t, '', 0, 'stdin')
  assert.equal(inert.invocation, null)
  assert.equal(inert.result.status, 0)
  assert.equal(inert.result.stdout, '')
  assert.equal(inert.result.stderr, '')
  for (const entry of ['direct', 'symlink']) {
    const { result, invocation } = invoke(t, '', 17, entry)
    assert.equal(invocation.command, 'sh')
    assert.equal(invocation.cwd, path.join(repoRoot, 'docs'))
    assert.equal(result.status, 1)
    assert.equal(result.stderr, 'command diagnosticprepare-docs: kio doc build --html failed\n')
  }
})
