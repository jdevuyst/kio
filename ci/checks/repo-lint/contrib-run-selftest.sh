#!/bin/sh
# Execute the actual maintainer and SHA-confinement gates with in-memory APIs.
# No PR checkout, external API requests, or contributor code execution.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
node - "$REPO_ROOT/.github/workflows/contrib-run.yml" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const workflow = fs.readFileSync(process.argv[2], 'utf8');
const starts = [...workflow.matchAll(/^          script: \|\n/gm)];
assert.equal(starts.length, 2, 'expected the authorization and identification scripts');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const scripts = starts.map(start => {
  const lines = workflow.slice(start.index + start[0].length).split('\n');
  const end = lines.findIndex(line => line !== '' && !line.startsWith('            '));
  return new AsyncFunction('github', 'context', 'core', lines.slice(0, end).map(line => line.slice(12)).join('\n'));
});
assert.match(workflow, /ref: \$\{\{ github\.event\.pull_request\.head\.sha \}\}/);
assert.match(workflow, /contrib-tests\.sh --impls=FULL_IMPL_MATRIX -- --toolchain-environment=development/);
const head = 'a'.repeat(40), base = 'b'.repeat(40);
const confined = [{ filename: 'test-data/contrib/alice-42/workdir/main.kio', status: 'modified' }];
async function check(fixture) {
  const failures = [], outputs = {}, calls = [];
  const files = fixture.files === undefined ? confined : fixture.files;
  const context = { repo: { owner: 'upstream', repo: 'project' }, payload: {
    sender: { login: 'maintainer' }, pull_request: { number: 7,
      head: { sha: head, repo: { full_name: 'alice/project' } }, base: { sha: base } }
  } };
  const request = args => {
    assert.equal(args.owner, 'upstream'); assert.equal(args.repo, 'project');
  };
  const github = {
    rest: {
      repos: {
        async getCollaboratorPermissionLevel(args) {
          request(args); assert.equal(args.username, 'maintainer');
          return { data: { permission: fixture.permission || 'write' } };
        },
        async compareCommitsWithBasehead(args) {
          request(args); calls.push('immutable comparison');
          assert.equal(args.basehead, `${base}...${head}`);
          assert.equal(args.page, 1); assert.equal(args.per_page, 1);
          if (fixture.apiError) throw new Error('comparison unavailable');
          return { data: { files, truncated: fixture.truncated || false } };
        }
      },
      pulls: {
        async get(args) {
          request(args); assert.equal(args.pull_number, 7);
          return { data: { head: { sha: fixture.staleHead ? 'c'.repeat(40) : head },
            base: { sha: fixture.staleBase ? 'c'.repeat(40) : base },
            labels: fixture.unsanctioned ? [] : [{ name: 'contrib case' }] } };
        },
        listFiles: 'mutable files'
      }
    },
    async paginate(method, args) {
      request(args); assert.equal(method, 'mutable files'); calls.push('mutable files');
      return fixture.mutableFiles || files;
    }
  };
  const core = { setFailed(message) { failures.push(message); },
    setOutput(key, value) { outputs[key] = value; } };
  try {
    await scripts[0](github, context, core);
    // Workflow steps use the default success() gate, so failed authorization
    // cannot reach Identify or checkout.
    if (!failures.length) await scripts[1](github, context, core);
  } catch (error) {
    failures.push(error.message);
  }
  const canExecute = failures.length === 0 && outputs.name !== undefined;
  assert.equal(canExecute, !fixture.rejected, `${fixture.name}: execution admission`);
  if (fixture.rejected) {
    assert(failures.length > 0, `${fixture.name}: explicit failure`);
    assert.deepEqual(outputs, {}, `${fixture.name}: no checkout/test output`);
    if (fixture.name === 'comparison at cap') assert.match(failures[0], /GitHub comparison.*300-file limit/);
  } else {
    assert.equal(outputs.name, 'alice-42');
    assert.deepEqual(calls, ['immutable comparison']);
  }
}
(async () => {
  let failed = 0;
  for (const fixture of [
    { name: 'same-head fork program' },
    { name: 'same-head fork library', files: [{ filename: 'test-data/contrib/alice-42/run.test-only', status: 'added' }] },
    { name: 'admin maintainer', permission: 'admin' },
    { name: 'triage rejected', permission: 'read', rejected: true },
    { name: 'missing live sanction', unsanctioned: true, rejected: true },
    { name: 'stale head', staleHead: true, rejected: true },
    { name: 'stale base', staleBase: true, rejected: true },
    { name: 'outside lane', files: [{ filename: 'ci/impl-toolchain.sh' }], rejected: true },
    { name: 'live files race cannot replace immutable comparison', files: [{ filename: 'ci/impl-toolchain.sh' }], mutableFiles: confined, rejected: true },
    { name: 'rename within case', files: [{ filename: 'test-data/contrib/alice-42/workdir/new.kio', previous_filename: confined[0].filename, status: 'renamed' }] },
    { name: 'rename from outside', files: [{ ...confined[0], previous_filename: 'ci/impl-toolchain.sh', status: 'renamed' }], rejected: true },
    { name: 'rename to outside', files: [{ filename: 'ci/impl-toolchain.sh', previous_filename: confined[0].filename, status: 'renamed' }], rejected: true },
    { name: 'rename between cases', files: [{ ...confined[0], previous_filename: 'test-data/contrib/alice-41/workdir/main.kio', status: 'renamed' }], rejected: true },
    { name: 'rename missing old path', files: [{ ...confined[0], status: 'renamed' }], rejected: true },
    { name: 'comparison at cap', files: Array(300).fill(confined[0]), rejected: true },
    { name: 'empty comparison', files: [], rejected: true },
    { name: 'missing comparison files', files: null, rejected: true },
    { name: 'truncated comparison', truncated: true, rejected: true },
    { name: 'comparison API unavailable', apiError: true, rejected: true }
  ]) {
    try { await check(fixture); console.log(`pass: ${fixture.name}`); }
    catch (error) { failed++; console.error(`FAIL: ${fixture.name}: ${error.message}`); }
  }
  assert.equal(failed, 0, 'contrib-run self-test failures');
})().catch(error => { console.error(error); process.exitCode = 1; });
NODE
