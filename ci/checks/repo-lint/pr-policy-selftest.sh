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

async function check({ name, dir, followUp, rejected, library = false,
  entryMode = '100644', ancestorMode = '040000', truncatedAt, missing = false }) {
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
            ...['README.md', library ? 'run.test-only' : 'run.args', 'expected.stdout', 'expected.exit', 'expected.stderr']
              .map(name => ({ name, type: 'file', ...(name === 'README.md' && entryMode === '160000'
                ? { submodule_git_url: 'https://example.invalid/module.git' } : {}) })),
            { name: 'workdir', type: 'dir', sha: 'tree' }
          ] };
          assert([`${casePath}/expected.exit`, `${casePath}/expected.stdout`].includes(args.path), 'unexpected content request');
          return { data: { type: 'file', content: Buffer.from(args.path.endsWith('expected.exit') ? '0\n' : '').toString('base64') } };
        }
      },
      git: {
        async getCommit(args) {
          request(args); assert.equal(args.commit_sha, 'head');
          return { data: { tree: { sha: 'root' } } };
        },
        async getTree(args) {
          request(args);
          if (args.tree_sha !== 'tree') {
            assert.equal(args.recursive, undefined);
            if (missing) throw Object.assign(new Error('missing tree'), { status: 404 });
            const ancestry = { root: ['test-data', 'test-data'], 'test-data': ['contrib', 'contrib'], contrib: [dir, 'case'] };
            const child = ancestry[args.tree_sha];
            const tree = child ? [{ path: child[0], sha: child[1],
              type: args.tree_sha === 'contrib' && ancestorMode !== '040000' ? 'blob' : 'tree',
              mode: args.tree_sha === 'contrib' ? ancestorMode : '040000' }]
              : ['README.md', library ? 'run.test-only' : 'run.args', 'expected.stdout', 'expected.exit', 'expected.stderr']
                .map(path => ({ path, type: path === 'README.md' && entryMode === '160000' ? 'commit' : 'blob',
                  mode: path === 'README.md' ? entryMode : '100644', sha: path }))
                .concat({ path: 'workdir', type: 'tree', mode: '040000', sha: 'tree' });
            assert(child || args.tree_sha === 'case', 'unexpected tree request');
            return { data: { truncated: truncatedAt === args.tree_sha, tree } };
          }
          assert.equal(args.recursive, 'true');
          return { data: { truncated: truncatedAt === 'tree', tree: [
            { path: 'app.pkg.kio', type: 'blob', mode: '100644', sha: 'package' },
            { path: 'main.kio', type: 'blob', mode: '100644', sha: 'module' }
          ] } };
        },
        async getBlob(args) {
          request(args);
          assert(['package', 'module'].includes(args.file_sha), 'unexpected blob request');
          const text = args.file_sha === 'package' ? 'package app;\n' : `module main;\n${library ? 'equiv identity x = x;\n' : ''}`;
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
    { name: 'wrong-owner follow-up', dir: 'bob-42', followUp: true, rejected: true },
    { name: 'regular executable file', dir: 'alice-42', entryMode: '100755', rejected: false },
    { name: 'regular library', dir: 'alice-42', library: true, rejected: false },
    { name: 'top-level gitlink new case', dir: 'alice-42', entryMode: '160000', rejected: true },
    { name: 'top-level gitlink follow-up', dir: 'alice-42', followUp: true, entryMode: '160000', rejected: true },
    { name: 'top-level symlink', dir: 'alice-42', entryMode: '120000', rejected: true },
    { name: 'unknown blob mode', dir: 'alice-42', entryMode: '100600', rejected: true },
    { name: 'symlink case ancestor', dir: 'alice-42', ancestorMode: '120000', rejected: true },
    { name: 'truncated ancestry tree', dir: 'alice-42', truncatedAt: 'root', rejected: true },
    { name: 'truncated case tree', dir: 'alice-42', truncatedAt: 'case', rejected: true },
    { name: 'truncated workdir tree', dir: 'alice-42', truncatedAt: 'tree', rejected: true },
    { name: 'missing ancestry tree', dir: 'alice-42', missing: true, rejected: true }
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
