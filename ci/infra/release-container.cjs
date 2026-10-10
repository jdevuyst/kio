'use strict';
// Release-only provenance/promotion adapter; never builds an image.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const cp = require('node:child_process');
const image = 'ghcr.io/jdevuyst/kio-devcontainer';
function command(tool, args) {
  return cp.execFileSync(tool, args, {encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024});
}
function inputs(env = process.env) {
  const {IMAGE_DIGEST: digest, SOURCE_SHA: source, VSIX_SHA256: vsix, VSIX_ARTIFACT_ID: artifact} = env;
  assert.match(digest, /^sha256:[a-f0-9]{64}$/);
  assert.match(source, /^[a-f0-9]{40}$/);
  assert.match(vsix, /^[a-f0-9]{64}$/);
  assert.match(artifact, /^[1-9][0-9]*$/);
  return {digest, source, vsix, artifact};
}
function inspect(ref, field) {
  return JSON.parse(command('docker', ['buildx', 'imagetools', 'inspect', ref, '--format', '{{json .' + field + '}}']));
}
function authenticate(index, configs, expected) {
  assert.equal(index.digest, expected.digest);
  for (const architecture of ['amd64', 'arm64']) {
    assert.match(index.manifests.find(m => m.platform?.os === 'linux' && m.platform?.architecture === architecture)?.digest, /^sha256:[a-f0-9]{64}$/);
    const labels = configs['linux/' + architecture].config.Labels;
    assert.equal(labels['org.opencontainers.image.source'], 'https://github.com/jdevuyst/kio');
    assert.equal(labels['org.opencontainers.image.revision'], expected.source);
    assert.equal(labels['io.kio.vsix.sha256'], expected.vsix);
    assert.equal(labels['io.kio.vsix.artifact-id'], expected.artifact);
  }
}
function receipt(file, expected) {
  const r = JSON.parse(fs.readFileSync(file));
  assert.equal(r.publishedProvenance.requestedImage, image + '@' + expected.digest);
  assert.equal(r.imageSourceCommit, expected.source);
  assert.equal(r.bakedVsix.bakedSha256, expected.vsix);
  assert.notEqual(r.imageSourceCommit, r.refreshCommit);
  assert.notEqual(r.tools[0].compiler.sha256, r.tools[1].compiler.sha256);
  assert.equal(r.phases.length, 19);
  assert(r.phases.every(p => p.code === 0 && !p.interrupted && !p.cleanupError));
  return {
    schema: 'kio-release-container-v1', ...expected,
    refreshCommit: r.refreshCommit, publishedProvenance: r.publishedProvenance,
    editor: r.editor, cliVersion: r.cliVersionOutput,
    compiler: r.tools.map(t => ({commit: t.commit, sha256: t.compiler.sha256, version: t.compiler.version})),
    phases: r.phases.map(p => ({phase: p.phase, code: p.code, elapsedMs: p.elapsedMs})),
    stages: r.stages.map(s => ({stage: s.stage, resources: s.resources,
      editor: {bakedSha256: s.editor.bakedSha256, activation: s.editor.activation, highlighting: s.editor.highlighting,
        hover: s.editor.hover, completion: s.editor.completion, diagnostic: s.editor.diagnostic, correction: s.editor.correction}})),
    injection: {restoredSemanticChecksPassed: r.injection.restoredSemanticChecksPassed},
    coverage: r.coverage, gaps: r.gaps
  };
}
function latestPolicy(source, current, isAncestor) {
  if (!current || current === source || isAncestor(current, source)) return 'advance';
  if (isAncestor(source, current)) return 'retain-newer';
  throw new Error('latest and candidate sources diverge; explicit publication decision required');
}
function ancestor(before, after) {
  const result = cp.spawnSync('git', ['merge-base', '--is-ancestor', before, after], {encoding: 'utf8'});
  if (result.error) throw result.error;
  assert([0, 1].includes(result.status), 'cannot establish source ancestry: ' + result.stderr);
  return result.status === 0;
}
function tagManifest(ref, read = inspect) {
  let descriptor;
  // Only failure to find the mutable tag itself can prove initial bootstrap.
  try {
    descriptor = read(ref, 'Manifest');
  } catch (error) {
    const stderr = String(error.stderr).trim();
    if (/manifest unknown|MANIFEST_UNKNOWN/.test(stderr) || stderr === 'ERROR: ' + ref + ': not found') return undefined;
    throw error;
  }
  assert.match(descriptor.digest, /^sha256:[a-f0-9]{64}$/);
  return descriptor;
}
function currentLatest(read = inspect) {
  const descriptor = tagManifest(image + ':latest', read);
  if (!descriptor) return undefined;
  const current = read(image + '@' + descriptor.digest, 'Image');
  const revisions = ['linux/amd64', 'linux/arm64'].map(p => current[p].config.Labels['org.opencontainers.image.revision']);
  assert.equal(revisions[0], revisions[1]);
  assert.match(revisions[0], /^[a-f0-9]{40}$/);
  return {digest: descriptor.digest, source: revisions[0]};
}
function promote() {
  const expected = inputs();
  assert.equal(command('git', ['rev-parse', 'HEAD']).trim(), expected.source);
  const ref = image + '@' + expected.digest;
  const index = inspect(ref, 'Manifest');
  authenticate(index, inspect(ref, 'Image'), expected);
  const latest = currentLatest();
  const policy = latestPolicy(expected.source, latest?.source, ancestor);
  const tags = ['sha-' + expected.source];
  if (policy === 'advance') tags.push('latest');
  // Write the rollback inputs before changing even one mutable tag. An absent
  // tag is distinct from a failed lookup, and a newer publication must survive.
  const before = Object.fromEntries(tags.map(tag => [tag, tagManifest(image + ':' + tag)?.digest ?? null]));
  const record = {schema: 'kio-container-promotion-v1', ...expected, index, tags, before, completed: false};
  assert(process.env.PROMOTION_RECEIPT, 'promotion requires a durable rollback receipt');
  fs.writeFileSync(process.env.PROMOTION_RECEIPT, JSON.stringify(record, null, 2));
  command('docker', ['buildx', 'imagetools', 'create', ...tags.flatMap(t => ['--tag', image + ':' + t]), ref]);
  for (const tag of tags) assert.equal(inspect(image + ':' + tag, 'Manifest').digest, expected.digest);
  if (policy === 'retain-newer') assert.equal(inspect(image + ':latest', 'Manifest').digest, latest.digest, 'newer latest must remain untouched');
  record.completed = true;
  record.latest = {policy, ...(policy === 'retain-newer' ? latest : {digest: expected.digest, source: expected.source})};
  fs.writeFileSync(process.env.PROMOTION_RECEIPT, JSON.stringify(record, null, 2));
  console.log(JSON.stringify(record, null, 2));
}
function selftest() {
  const expected = inputs({IMAGE_DIGEST: 'sha256:' + 'a'.repeat(64), SOURCE_SHA: 'b'.repeat(40), VSIX_SHA256: 'c'.repeat(64), VSIX_ARTIFACT_ID: '123'});
  assert.throws(() => inputs({...process.env, IMAGE_DIGEST: 'latest'}));
  const index = {digest: expected.digest, manifests: ['amd64', 'arm64'].map(architecture => ({digest: 'sha256:' + 'd'.repeat(64), platform: {os: 'linux', architecture}}))};
  const configs = Object.fromEntries(['linux/amd64', 'linux/arm64'].map(p => [p, {config: {Labels: {
    'org.opencontainers.image.source': 'https://github.com/jdevuyst/kio',
    'org.opencontainers.image.revision': expected.source, 'io.kio.vsix.sha256': expected.vsix, 'io.kio.vsix.artifact-id': expected.artifact
  }}}]));
  authenticate(index, configs, expected);
  for (const key of ['digest', 'source', 'vsix', 'artifact']) assert.throws(() => authenticate(index, configs, {...expected, [key]: 'wrong'}));
  delete configs['linux/arm64'];
  assert.throws(() => authenticate(index, configs, expected));
  console.log('release-container selftest: immutable index and both-platform source/payload mismatch rejection passed');
}
if (require.main === module) {
  switch (process.argv[2]) {
    case 'promote': promote(); break;
    case 'receipt': console.log(JSON.stringify(receipt(process.argv[3], inputs()), null, 2)); break;
    case 'refresh': {
      const source = inputs().source;
      const candidates = command('git', ['rev-list', '--first-parent', source + '^']).trim().split('\n');
      const refresh = candidates.find(commit => command('git', ['diff', '--name-only', source, commit, '--', 'kio-rs/src', 'kio-rs/build.rs', 'kio-rs/Cargo.toml', 'kio-rs/Cargo.lock']).trim());
      assert(refresh, 'compiler-changing ancestor required');
      console.log(refresh); break;
    }
    case 'selftest': selftest(); break;
    default: throw new Error('expected promote, receipt, refresh or selftest');
  }
}
module.exports = {inputs, authenticate, receipt, latestPolicy, currentLatest, tagManifest, inspect};
