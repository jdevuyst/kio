'use strict';
// Focused release-only graph/identity tests; no image build or external writes.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {inputs, authenticate, latestPolicy, currentLatest, tagManifest} = require('./release-container.cjs');
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
function simulate(graph, failures = []) {
  const state = {};
  for (const [job, {needs}] of Object.entries(graph)) {
    assert(needs.every(dep => dep in state), 'graph must be topological');
    state[job] = needs.every(dep => state[dep] === 'success') ? (failures.includes(job) ? 'failure' : 'success') : 'skipped';
  }
  return state;
}
function checkGraph(prepare, container, acceptance, recovery) {
  assert.deepEqual(container.acceptance.needs, ['build']);
  assert.match(container.acceptance.text, /uses: .*accept-container.yml/);
  assert.deepEqual(acceptance.promote.needs, ['accept']);
  assert.deepEqual(prepare.ready.needs, ['package', 'container', 'crate', 'website']);
  assert.deepEqual(prepare.container.needs, ['package']);
  assert.deepEqual(prepare.website.needs, ['container']);
  assert.deepEqual(recovery.container.needs, ['verify', 'credentials']);
  assert.match(recovery.container.text, /uses: .*accept-container.yml/);
  assert.deepEqual(recovery.publish.needs, ['verify', 'container', 'website']);
  assert.deepEqual(recovery.deploy.needs, ['verify', 'website', 'publish']);
}
const release = jobs(read('publish-release'));
const prepare = jobs(read('prepare-release'));
const container = jobs(read('devcontainer-candidate'));
const acceptance = jobs(read('accept-container'));
const recovery = jobs(read('release-recovery'));
checkGraph(prepare, container, acceptance, recovery);
for (const failed of ['package', 'container', 'crate', 'website'])
  assert.notEqual(simulate(prepare, [failed]).ready, 'success');
for (const failed of ['verify', 'credentials', 'container', 'website'])
  assert.equal(simulate(recovery, [failed]).publish, 'skipped');
assert.equal(simulate(acceptance, ['accept']).promote, 'skipped');
assert.equal(simulate(release, ['crate']).vscode, 'success');
const registries = jobs(read('publish-vscode'));
assert.equal(simulate(registries, ['marketplace'])['open-vsx'], 'success');
assert.equal(simulate(registries, ['open-vsx']).marketplace, 'success');
const bad = structuredClone(acceptance); bad.promote.needs = [];
assert.throws(() => checkGraph(prepare, container, bad, recovery));
const unguarded = structuredClone(recovery); unguarded.publish.needs = ['verify'];
assert.throws(() => checkGraph(prepare, container, acceptance, unguarded));
assert(!read('release').includes('custom-publish-crate:'));
assert(!read('release').includes('custom-publish-vscode:'));
assert.equal((read('publish-vscode').match(/sha256sum --check --strict/g) || []).length, 2);
assert.equal((read('publish-vscode').match(/run-id: \$\{\{ inputs.artifact-run-id \|\| github.run_id \}\}/g) || []).length, 2);
assert(!read('publish-vscode').includes('npm run package'));
assert(!read('release-recovery').includes('build-push-action'));
assert.match(read('accept-container'), /PROMOTION_RECEIPT:/);
const {validate} = require('./release-recovery.cjs');
const metadata = {tag: 'releases/v1.2.3', release: {tag_name: 'releases/v1.2.3', immutable: true, draft: false, prerelease: false},
  source: 'b'.repeat(40), run: {id: 123, run_attempt: 2, head_sha: 'b'.repeat(40), repository: {full_name: 'jdevuyst/kio'},
    path: '.github/workflows/release.yml', event: 'push', status: 'completed'},
  jobs: ['host', 'custom-publish-release / package / Package extension', 'custom-publish-release / container / build']
    .map(name => ({name, conclusion: 'success'})),
  artifacts: [{name: 'kio-vsix-123-2', id: 456, expired: false}]};
assert.equal(validate(metadata).id, 456);
for (const change of [
  m => {m.release.immutable = false;}, m => {m.run.head_sha = 'c'.repeat(40);},
  m => {m.run.repository.full_name = 'other/repo';}, m => {m.run.event = 'pull_request';},
  m => {m.jobs[1].conclusion = 'skipped';}, m => {m.artifacts[0].expired = true;},
  m => {m.artifacts[0].name = 'kio-vsix-123-1';},
  m => {m.jobs.push({name: 'custom-publish-release / crate / publish', conclusion: 'success'});},
]) { const invalid = structuredClone(metadata); change(invalid); assert.throws(() => validate(invalid)); }
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
const missingLatest = Object.assign(new Error('registry tag missing'), {
  stderr: 'ERROR: ghcr.io/jdevuyst/kio-devcontainer:latest: not found\n'
});
assert.equal(currentLatest(() => {throw missingLatest;}), undefined, 'buildx absent-tag response permits bootstrap');
const sourceTag = 'ghcr.io/jdevuyst/kio-devcontainer:sha-' + expected.source;
assert.equal(tagManifest(sourceTag, () => {throw Object.assign(new Error('missing source tag'), {
  stderr: 'ERROR: ' + sourceTag + ': not found\n'
});}), undefined, 'first promotion records an absent source tag');
assert.equal(tagManifest(sourceTag, () => index).digest, expected.digest);
assert.throws(() => tagManifest(sourceTag, () => ({digest: 'invalid'})));
for (const stderr of [
  'ERROR: ghcr.io/other/image:latest: not found\n',
  'ERROR: ghcr.io/jdevuyst/kio-devcontainer:latest: unauthorized\n',
  'ERROR: ghcr.io/jdevuyst/kio-devcontainer:latest: not found\nconnection failed',
  'ERROR: executable not found\n',
]) assert.throws(() => currentLatest(() => {throw Object.assign(new Error('lookup failed'), {stderr});}));
let lookups = 0;
assert.throws(() => currentLatest(() => {
  if (++lookups === 1) return index;
  throw missing;
}), 'failure reading an existing immutable index/config must not permit bootstrap');
assert.throws(() => currentLatest(() => {throw new Error('network timeout');}));
console.log('release-container graph selftest: binary host, acceptance failure blocking, independent leaves, single VSIX and identity tampering passed');
