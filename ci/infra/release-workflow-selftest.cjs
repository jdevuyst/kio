const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { augment, assertFresh, inputs } = require('./release-workflow.cjs');
const root = path.resolve(__dirname, '../..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');
const actual = read('.github/workflows/release.yml');
const dependency = '      - custom-release-credentials\n';
const guard = "needs.custom-release-credentials.result == 'success' && ";
const verified = "needs.custom-release-credentials.outputs.verified == 'true' && ";
const raw = actual.replace('  host:\n    needs:\n' + dependency, '  host:\n    needs:\n').replace(guard, '').replaceAll(verified, '');
assert.equal(augment(raw), actual);
assert.throws(() => augment(actual));
assert.throws(() => augment(raw.replace('  host:', '  upstream-renamed-host:')));
assert.throws(() => augment(raw.replace("always() && needs.plan.result", "always() && upstream.result")));
assert.throws(() => augment(raw.replace('  custom-release-credentials:', '  removed-hook:')));
assert.throws(() => augment(raw.replace(dependency, '')));
assertFresh(augment(raw), actual);
assert.throws(() => assertFresh(augment(raw), actual.replace('ubuntu-22.04', 'ubuntu-24.04')));
const host = actual.match(/^  host:\n([\s\S]*?)(?=^  [\w-]+:)/m)[1];
assert(host.includes(dependency));
const condition = host.match(/^    if: (.*)$/m)[1];
// Unmodified upstream accepts skipped builds after a failed plan prerequisite:
// this control proves the strict guard is what prevents publication.
const before = condition.replace(guard, '').replace(verified, '')
  .replace(/\$\{\{|\}\}/g, '').replace('always()', 'true')
  .replace(/needs\.plan\.result/g, "'success'").replace(/needs\.plan\.outputs\.publishing/g, "'true'")
  .replace(/needs\.build-(?:local|global)-artifacts\.result/g, "'skipped'");
assert.equal(Function(`return (${before});`)(), true, 'before-fix bypass must be observable');
for (const status of ['failure', 'cancelled', 'skipped', 'success']) for (const output of ['', 'true']) {
  const expression = condition.replace(/\$\{\{|\}\}/g, '').replace('always()', 'true')
    .replace(/needs\.custom-release-credentials\.result/g, JSON.stringify(status))
    .replace(/needs\.custom-release-credentials\.outputs\.verified/g, JSON.stringify(output))
    .replace(/needs\.plan\.result/g, "'success'").replace(/needs\.plan\.outputs\.publishing/g, "'true'")
    .replace(/needs\.build-(?:local|global)-artifacts\.result/g, "'skipped'");
  assert.equal(Function(`return (${expression});`)(), status === 'success' && output === 'true', `${status}/${output}`);
}
const build = actual.match(/^  build-local-artifacts:\n([\s\S]*?)(?=^  [\w-]+:)/m)[1];
assert(build.includes('    if: ${{ ' + verified));
assert(preflightOutputIsExecutionWitness());
function preflightOutputIsExecutionWitness() {
  const text = read('.github/workflows/release-credentials.yml');
  return text.includes('value: ${{ jobs.check.outputs.verified }}') &&
    text.includes('verified: ${{ steps.verify.outputs.verified }}') &&
    /node ci\/infra\/release-credentials.cjs\n\s+echo 'verified=true'/.test(text);
}
const publisher = actual.match(/^  custom-publish-release:\n([\s\S]*?)(?=^  [\w-]+:)/m)[1];
assert(publisher.includes('      - host\n') && !publisher.includes('always()'));
assert(actual.includes("always() && needs.host.result == 'success'"));
const preflight = read('.github/workflows/release-credentials.yml');
assert(!preflight.includes('pull_request') && !preflight.includes('schedule:'));
assert(preflight.includes("github.event_name == 'push' && github.ref_type == 'tag'"));
assert(preflight.includes('node ci/infra/release-workflow.cjs --check'));
assert(preflight.indexOf('--check') < preflight.indexOf('CARGO_REGISTRY_TOKEN:'));
const exists = file => fs.existsSync(path.join(root, file));
const list = dir => fs.readdirSync(path.join(root, dir));
inputs(read, exists, list);
for (const [file, mutate] of [
  ['dist-workspace.toml', text => text.replace('cargo:kio-rs/dist', 'cargo:new-app')],
  ['dist-workspace.toml', text => text.replace('members =', '# members =') + '\nmembers = ["cargo:new-app"]\n'],
  ['dist-workspace.toml', text => text + '\ninclude = ["missing-input"]\n'],
  ['dist-workspace.toml', text => text + '\ngithub-build-setup = "missing.yml"\n'],
  ['kio-rs/dist/Cargo.toml', text => text.replace('../src/main.rs', '../src/new.rs')],
  ['kio-rs/Cargo.toml', text => text.replace('README.md', 'OTHER.md')],
]) assert.throws(() => inputs(name => name === file ? mutate(read(name)) : read(name), exists, list));
assert.throws(() => inputs(read, file => file === 'kio-rs/dist/tests' || exists(file), list));
assert.throws(() => inputs(read, exists, () => ['kio-prime.rs', 'new.rs']));
console.log('release workflow freshness and failed/cancelled/skipped preflight guards passed');
