#!/bin/sh
# Exercise the actual PR policy script with in-memory GitHub responses.
# Portable POSIX shell + Node; no network, checkout of PR code, or API writes.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

node - "$REPO_ROOT/.github/workflows/pr-policy-check.yml" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const workflow = fs.readFileSync(process.argv[2], 'utf8');
assert.equal((workflow.match(/^      - uses: actions\/github-script@/gm) || []).length, 1);
const starts = [...workflow.matchAll(/^          script: \|\n/gm)];
assert.equal(starts.length, 1, 'expected one literal GitHub policy script');
const lines = workflow.slice(starts[0].index + starts[0][0].length).split('\n');
assert(lines.every(line => line === '' || line.startsWith('            ')),
  'policy script must be the final indented workflow block');
const source = lines.map(line => line.slice(12)).join('\n');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const policy = new AsyncFunction('github', 'context', 'core', source);

async function check({ name, dir, followUp, rejected }) {
  const casePath = `test-data/contrib/${dir}`;
  const added = [], removed = [], failures = [], comments = [];
  const context = {
    repo: { owner: 'upstream', repo: 'project' },
    serverUrl: 'https://example.invalid',
    payload: {
      action: 'opened',
      pull_request: {
        number: 99, draft: false, user: { login: 'Alice' },
        head: { sha: 'head' }, base: { sha: 'base' }
      }
    }
  };
  const request = args => {
    assert.equal(args.owner, 'upstream');
    assert.equal(args.repo, 'project');
  };
  const listFiles = Symbol('listFiles'), listComments = Symbol('listComments');
  const github = {
    async graphql(_query, args) {
      request(args);
      assert.equal(args.number, 99);
      return { repository: { pullRequest: { closingIssuesReferences: {
        nodes: followUp ? [] : [{
          number: 42, state: 'OPEN', labels: { nodes: [{ name: 'contrib case' }] }
        }]
      } } } };
    },
    async paginate(method, args) {
      request(args);
      if (method === listFiles) {
        assert.equal(args.pull_number, 99);
        return [{ filename: `${casePath}/workdir/main.kio` }];
      }
      assert.equal(method, listComments, 'unexpected paginated API');
      assert.equal(args.issue_number, 99);
      return [];
    },
    rest: {
      pulls: { listFiles },
      issues: {
        listComments,
        async addLabels(args) {
          request(args); assert.equal(args.issue_number, 99);
          added.push(...args.labels);
        },
        async removeLabel(args) {
          request(args); assert.equal(args.issue_number, 99);
          removed.push(args.name);
        },
        async createComment(args) {
          request(args); assert.equal(args.issue_number, 99);
          comments.push(args.body);
        }
      },
      repos: {
        async getContent(args) {
          request(args);
          if (args.ref === 'base' && args.path === casePath) {
            if (followUp) return { data: [] };
            throw Object.assign(new Error('new case'), { status: 404 });
          }
          assert.equal(args.ref, 'head');
          if (args.path === casePath) return { data: [
            ...['README.md', 'run.args', 'expected.stdout', 'expected.exit', 'expected.stderr']
              .map(name => ({ name, type: 'file' })),
            { name: 'workdir', type: 'dir', sha: 'tree' }
          ] };
          assert.equal(args.path, `${casePath}/expected.exit`, 'unexpected content request');
          return { data: { type: 'file', content: Buffer.from('0\n').toString('base64') } };
        }
      },
      git: {
        async getTree(args) {
          request(args); assert.equal(args.tree_sha, 'tree');
          assert.equal(args.recursive, 'true');
          return { data: { truncated: false, tree: [
            { path: 'app.pkg.kio', type: 'blob', mode: '100644', sha: 'package' },
            { path: 'main.kio', type: 'blob', mode: '100644', sha: 'module' }
          ] } };
        },
        async getBlob(args) {
          request(args);
          assert(['package', 'module'].includes(args.file_sha), 'unexpected blob request');
          const text = args.file_sha === 'package' ? 'package app;\n' : 'module main;\n';
          return { data: { encoding: 'base64', content: Buffer.from(text).toString('base64') } };
        }
      }
    }
  };
  await policy(github, context, {
    info() {},
    warning(message) { throw new Error(message); },
    setFailed(message) { failures.push(message); }
  });
  assert.equal(failures.length, rejected ? 1 : 0, `${name}: policy verdict`);
  assert.deepEqual(added, rejected ? [] : ['contrib case'], `${name}: added labels`);
  assert.deepEqual(removed, rejected ? ['contrib case'] : [], `${name}: removed labels`);
  assert.equal(comments.length, rejected ? 1 : 0, `${name}: feedback comments`);
}

(async () => {
  let failed = 0;
  for (const fixture of [
    { name: 'lowercase new case / mixed-case login', dir: 'alice-42', followUp: false, rejected: false },
    { name: 'uppercase new case', dir: 'Alice-42', followUp: false, rejected: true },
    { name: 'uppercase follow-up', dir: 'Alice-42', followUp: true, rejected: true },
    { name: 'lowercase follow-up / no linked issue', dir: 'alice-42', followUp: true, rejected: false },
    { name: 'wrong-owner follow-up', dir: 'bob-42', followUp: true, rejected: true }
  ]) {
    try {
      await check(fixture);
      console.log(`pass: ${fixture.name}`);
    } catch (error) {
      failed++;
      console.error(`FAIL: ${fixture.name}: ${error.message}`);
    }
  }
  assert.equal(failed, 0, 'PR policy self-test failures');
})().catch(error => { console.error(error); process.exitCode = 1; });
NODE
