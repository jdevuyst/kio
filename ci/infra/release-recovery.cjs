'use strict';
// Resume frozen artifacts using current automation, never a rebuilt payload.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const {execFileSync} = require('node:child_process');
const {authenticate, inspect} = require('./release-container.cjs');
const repository = 'jdevuyst/kio';
function validate({tag, release, source, run, jobs, artifacts}) {
  assert.match(tag, /^releases\/v\d+\.\d+\.\d+$/);
  assert.equal(release.tag_name, tag);
  assert.equal(release.immutable, true, 'recovery requires an already immutable release');
  assert.equal(release.draft, false);
  assert.equal(release.prerelease, false, 'this recovery route publishes stable releases only');
  assert.equal(run.repository.full_name, repository);
  assert.equal(run.path, '.github/workflows/release.yml');
  assert.equal(run.event, 'push');
  assert.equal(run.head_sha, source);
  assert.equal(run.status, 'completed');
  assert(jobs.some(j => ['host', 'Point of no return — publish immutable GitHub release'].includes(j.name) && j.conclusion === 'success'));
  assert(jobs.some(j => j.name.endsWith(' / package / Package extension') && j.conclusion === 'success'));
  assert(jobs.some(j => j.name.endsWith(' / container / build') && j.conclusion === 'success'));
  assert(!jobs.some(j => /\/(?: crate(?: \/ publish)?| vscode(?: \/.*)?)$/.test(j.name) && j.conclusion === 'success'),
    'a registry already succeeded; retry only the failed leaf, not this recovery coordinator');
  const matches = artifacts.filter(a => a.name === `kio-vsix-${run.id}-${run.run_attempt}`);
  assert.equal(matches.length, 1, 'exact original VSIX artifact required');
  assert.equal(matches[0].expired, false);
  return matches[0];
}
async function main() {
  const tag = process.env.RELEASE_TAG;
  const id = process.env.RELEASE_RUN_ID;
  assert.match(tag, /^releases\/v\d+\.\d+\.\d+$/);
  assert.match(id, /^[1-9][0-9]*$/);
  const command = (tool, args) => execFileSync(tool, args, {encoding: 'utf8', timeout: 120000, maxBuffer: 16 * 1024 * 1024});
  const request = endpoint => fetch(`https://api.github.com/repos/${repository}/${endpoint}`, {
    headers: {Authorization: `Bearer ${process.env.GH_TOKEN}`, 'User-Agent': 'kio-release-recovery'},
    redirect: 'manual', signal: AbortSignal.timeout(30000),
  });
  const api = async endpoint => {
    const response = await request(endpoint);
    assert.equal(response.status, 200, 'GitHub inventory query failed');
    return response.json();
  };
  const release = await api('releases/tags/' + encodeURIComponent(tag));
  const source = (await api('commits/' + encodeURIComponent(tag))).sha;
  assert.match(source, /^[a-f0-9]{40}$/);
  const run = await api('actions/runs/' + id);
  const jobs = await api(`actions/runs/${id}/attempts/${run.run_attempt}/jobs?per_page=100`);
  const artifacts = await api(`actions/runs/${id}/artifacts?per_page=100`);
  assert(jobs.total_count <= 100 && artifacts.total_count <= 100, 'inventory exceeds bounded recovery query');
  const artifact = validate({tag, release, source, run, jobs: jobs.jobs, artifacts: artifacts.artifacts});
  command('git', ['merge-base', '--is-ancestor', source, 'HEAD']);
  const manifest = JSON.parse(command('git', ['show', source + ':tools/vscode-kio/package.json']));
  assert.equal('releases/v' + manifest.version, tag);
  assert.equal(manifest.publisher, 'kio-lang');
  assert.equal(manifest.name, 'kio');
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'kio-release-recovery-'));
  try {
    const redirect = await request(`actions/artifacts/${artifact.id}/zip`);
    assert.equal(redirect.status, 302, 'artifact download must provide a signed URL');
    const url = new URL(redirect.headers.get('location'));
    assert.equal(url.protocol, 'https:');
    // Fetch the signed storage URL without forwarding the GitHub credential.
    const download = await fetch(url, {signal: AbortSignal.timeout(120000)});
    assert.equal(download.status, 200, 'original artifact download failed');
    const zip = path.join(temp, 'artifact.zip');
    fs.writeFileSync(zip, Buffer.from(await download.arrayBuffer()));
    const payload = execFileSync('unzip', ['-p', zip, 'kio.vsix'], {timeout: 30000, maxBuffer: 128 * 1024 * 1024});
    const vsix = crypto.createHash('sha256').update(payload).digest('hex');
    const image = 'ghcr.io/jdevuyst/kio-devcontainer';
    const index = inspect(`${image}:candidate-${id}-${run.run_attempt}`, 'Manifest');
    const expected = {source, digest: index.digest, vsix, artifact: String(artifact.id)};
    authenticate(index, inspect(image + '@' + index.digest, 'Image'), expected);
    const outputs = {'source-sha': source, digest: index.digest, 'artifact-id': String(artifact.id),
      sha256: vsix, 'artifact-run-id': id, plan: JSON.stringify({announcement_tag: tag, announcement_is_prerelease: false})};
    fs.appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(outputs).map(([k, v]) => `${k}=${v}\n`).join(''));
    console.log(JSON.stringify({release: release.id, tag, originalRun: id, ...expected}, null, 2));
  } finally { fs.rmSync(temp, {recursive: true, force: true}); }
}
module.exports = {validate};
if (require.main === module) main().catch(error => { console.error(error.message); process.exitCode = 1; });
