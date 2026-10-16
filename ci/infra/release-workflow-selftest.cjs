const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { augment, assertFresh, inputs } = require('./release-workflow.cjs');
const root = path.resolve(__dirname, '../..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');
const actual = read('.github/workflows/release.yml');
// Both credential success and complete preparation are execution witnesses,
// not merely needs-edges that an always() job could bypass after a skip.
const host = actual.match(/^  host:\n([\s\S]*?)(?=^  [\w-]+:)/m)[1];
const condition = host.match(/^    if: (.*)$/m)[1];
function evaluate(condition, overrides = {}, cancelled = false) {
  const values = {'plan.result': 'success', 'plan.outputs.publishing': 'true',
    'custom-release-credentials.result': 'success', 'custom-release-credentials.outputs.verified': 'true',
    'build-local-artifacts.result': 'success', 'build-global-artifacts.result': 'success',
    'custom-prepare-release.result': 'success', 'custom-prepare-release.outputs.ready': 'true', ...overrides};
  const expression = condition.replace(/\$\{\{|\}\}/g, '').replace('always()', 'true')
    .replace('cancelled()', JSON.stringify(cancelled))
    .replace(/needs\.([\w.-]+)/g, (_, key) => {assert(key in values, key); return JSON.stringify(values[key]);});
  return Function('return (' + expression + ')')();
}
assert.equal(evaluate(condition), true);
assert.equal(evaluate(condition, {}, true), false, 'run cancellation must stop hosting even after every prerequisite succeeded');
for (const job of ['plan', 'custom-release-credentials', 'build-local-artifacts', 'build-global-artifacts', 'custom-prepare-release'])
  for (const status of ['failure', 'cancelled', 'skipped'])
    assert.equal(evaluate(condition, {[job + '.result']: status}), false, job + '/' + status);
for (const witness of ['custom-release-credentials.outputs.verified', 'custom-prepare-release.outputs.ready', 'plan.outputs.publishing'])
  for (const output of ['', 'false']) assert.equal(evaluate(condition, {[witness]: output}), false);
assert(host.includes('      - custom-prepare-release\n'));
assert(host.includes('Point of no return'));
assert.equal((actual.match(/uses: \.\/\.github\/actions\/setup-toolchains/g) || []).length, 4);
assert(!/sh\.rustup\.rs|cargo-dist-installer|cargo-dist-cache|matrix\.install_dist\.run/.test(actual));
// Removing the acceptance guard allows hosting after preparation fails.
const before = condition.replace(" && needs.custom-prepare-release.result == 'success' && needs.custom-prepare-release.outputs.ready == 'true'", '');
assert.equal(evaluate(before, {'custom-prepare-release.result': 'failure'}), true);
assert.throws(() => augment(actual), 'already augmented source must fail');
assertFresh(actual, actual);
assert.throws(() => assertFresh(actual, actual.replace('ubuntu-22.04', 'ubuntu-24.04')));
const publisher = actual.match(/^  custom-publish-release:\n([\s\S]*?)(?=^  [\w-]+:)/m)[1];
assert(publisher.includes('      - host\n') && publisher.includes('      - custom-prepare-release\n'));
assert(publisher.includes('artifact-id: ${{ needs.custom-prepare-release.outputs.artifact-id }}'));
assert(publisher.includes('sha256: ${{ needs.custom-prepare-release.outputs.sha256 }}'));
const preflight = read('.github/workflows/release-credentials.yml');
assert(!preflight.includes('pull_request') && !preflight.includes('schedule:'));
assert(preflight.includes("github.event_name == 'push' && github.ref_type == 'tag'"));
assert(preflight.includes('node ci/infra/release-workflow.cjs --check'));
assert(preflight.indexOf('--check') < preflight.indexOf('          CARGO_REGISTRY_TOKEN:'));
assert(preflight.includes('value: ${{ jobs.check.outputs.verified }}') &&
  /node ci\/infra\/release-credentials.cjs\n\s+echo 'verified=true'/.test(preflight));
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
console.log('release workflow freshness and failed/cancelled/skipped preparation guards passed');
