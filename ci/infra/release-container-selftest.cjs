'use strict';
// Focused release-only graph/identity tests; no image build or external writes.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {inputs, authenticate, latestPolicy, currentLatest} = require('./release-container.cjs');
const root = path.resolve(__dirname, '../..');
const read = name => fs.readFileSync(path.join(root, '.github/workflows', name + '.yml'), 'utf8');
function jobs(text) {
  const result = {};
  const body = text.split(/^jobs:\s*$/m)[1];
  assert(body, 'workflow must define jobs');
  for (const match of body.matchAll(/^  ([a-z][a-z0-9-]*):\n([\s\S]*?)(?=^  [a-z][a-z0-9-]*:|$(?![\s\S]))/gm)) {
    const dependencies = match[2].match(/^    needs: (.+)$/m)?.[1] || '';
    result[match[1]] = {text: match[2], needs: dependencies.replace(/[\[\] ]/g, '').split(',').filter(Boolean)};
  }
  return result;
}
function checkGraph(release, container) {
  for (const publisher of ['crate', 'vscode']) assert(release[publisher].needs.includes('container'), publisher + ' must await accepted promotion');
  assert.deepEqual(release.container.needs, ['package']);
  assert.match(release.package.text, /uses: .*package-vscode.yml/);
  assert.deepEqual(container.accept.needs, ['build']);
  assert.deepEqual(container.promote.needs, ['build', 'accept']);
}
function simulate(graph, failures = []) {
  const state = {};
  for (const [job, {needs}] of Object.entries(graph)) {
    assert(needs.every(dep => dep in state), 'graph must be topological');
    state[job] = needs.every(dep => state[dep] === 'success') ? (failures.includes(job) ? 'failure' : 'success') : 'skipped';
  }
  return state;
}
const release = jobs(read('publish-release'));
const container = jobs(read('devcontainer-candidate'));
checkGraph(release, container);
for (const failed of ['build', 'accept', 'promote']) {
  const candidate = simulate(container, [failed]);
  assert.notEqual(candidate.promote, 'success');
  const publishers = simulate(release, ['container']);
  assert.equal(publishers.crate, 'skipped');
  assert.equal(publishers.vscode, 'skipped');
}
assert.equal(simulate(release, ['crate']).vscode, 'success', 'independent leaf failure must not block editor publishers');
const registries = jobs(read('publish-vscode'));
assert.equal(simulate(registries, ['marketplace'])['open-vsx'], 'success');
assert.equal(simulate(registries, ['open-vsx']).marketplace, 'success');
for (const publisher of ['crate', 'vscode']) {
  const bad = structuredClone(release); bad[publisher].needs = ['package'];
  assert.throws(() => checkGraph(bad, container), 'ungated publishers must be rejected');
}
const bad = structuredClone(container); bad.promote.needs = ['build'];
assert.throws(() => checkGraph(release, bad), 'acceptance bypass must be rejected');
assert.match(read('release'), /custom-publish-release:\n    needs:\n      - plan\n      - host/);
assert(!read('release').includes('custom-publish-crate:'));
assert(!read('release').includes('custom-publish-vscode:'));
assert.match(fs.readFileSync(path.join(root, 'dist-workspace.toml'), 'utf8'), /publish-jobs = \["\.\/publish-release"\]/);
assert.equal((read('publish-vscode').match(/sha256sum --check --strict/g) || []).length, 2);
assert(!read('publish-vscode').includes('npm run package'));
assert(!read('devcontainer-candidate').includes('value=latest'));
const expected = inputs({IMAGE_DIGEST: 'sha256:' + 'a'.repeat(64), SOURCE_SHA: 'b'.repeat(40), VSIX_SHA256: 'c'.repeat(64), VSIX_ARTIFACT_ID: '123'});
const index = {digest: expected.digest, manifests: ['amd64', 'arm64'].map(architecture => ({digest: 'sha256:' + 'd'.repeat(64), platform: {os: 'linux', architecture}}))};
const labels = {'org.opencontainers.image.source': 'https://github.com/jdevuyst/kio', 'org.opencontainers.image.revision': expected.source, 'io.kio.vsix.sha256': expected.vsix, 'io.kio.vsix.artifact-id': expected.artifact};
const configs = {'linux/amd64': {config: {Labels: labels}}, 'linux/arm64': {config: {Labels: {...labels}}}};
authenticate(index, configs, expected);
for (const key of Object.keys(labels)) {
  const corrupt = structuredClone(configs); corrupt['linux/arm64'].config.Labels[key] = 'corrupt';
  assert.throws(() => authenticate(index, corrupt, expected));
}
const history = ['old', 'candidate', 'newer'];
const ancestor = (before, after) => history.includes(before) && history.includes(after) && history.indexOf(before) <= history.indexOf(after);
assert.equal(latestPolicy('candidate', undefined, ancestor), 'advance');
assert.equal(latestPolicy('candidate', 'candidate', ancestor), 'advance');
assert.equal(latestPolicy('candidate', 'old', ancestor), 'advance');
assert.equal(latestPolicy('candidate', 'newer', ancestor), 'retain-newer');
assert.throws(() => latestPolicy('candidate', 'divergent', ancestor));
const missing = Object.assign(new Error('registry manifest missing'), {stderr: 'MANIFEST_UNKNOWN'});
assert.equal(currentLatest(() => {throw missing;}), undefined, 'absent mutable tag permits bootstrap');
let lookups = 0;
assert.throws(() => currentLatest(() => {
  if (++lookups === 1) return index;
  throw missing;
}), 'failure reading an existing immutable index/config must not permit bootstrap');
assert.throws(() => currentLatest(() => {throw new Error('network timeout');}));
console.log('release-container graph selftest: binary host, acceptance failure blocking, independent leaves, single VSIX and identity tampering passed');
