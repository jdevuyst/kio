const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const cp = require('node:child_process');
const { createRequire } = require('node:module');

const control = '/acceptance';
const workspace = '/workspaces/kio';
const id = 'kio-lang.kio';
const hash = data => crypto.createHash('sha256').update(data).digest('hex');
const read = file => JSON.parse(fs.readFileSync(file, 'utf8'));
const write = (file, data) => fs.writeFileSync(file, JSON.stringify(data, null, 2) + '\n');
const command = (bin, args, options = {}) => cp.execFileSync(bin, args, { encoding: 'utf8', ...options }).trim();
const dependencies = root => createRequire(path.join(root, 'tools/vscode-kio/package.json'));
const workspaces = ['kio-rs', 'ci/infra/kio-ci-scheduler-rs', 'ci/infra/kio-gen-rs', 'ci/infra/kio-prime-check-rs', 'ci/infra/kio-test-runner-rs'];
const fixtureSource = 'module main;\n\nfn identity(x: .) { x }\n\nfn caller() { identity(()) }\n';

function nonroot(uid) { assert.notEqual(uid, 0, 'remote execution must be nonroot'); }
function ownership(file) {
  assert.equal(Number(fs.readFileSync(file, 'utf8').trim()), process.getuid(),
    'host checkout owner must match published vscode UID; UID-adjustment image builds are excluded');
}
function containerImage(image, container, provenance) {
  const observed = read(container)[0];
  const allowed = [read(image)[0].Id];
  if (provenance) {
    const info = read(provenance);
    allowed.push(info.imageConfig, info.platformManifest);
    assert.equal(observed.Config.Image, info.requestedImage, 'container must request the exact published reference');
    assert.ok(allowed.includes(observed.Image), 'container must execute the published image without a derived image build');
    const descriptor = observed.ImageManifestDescriptor;
    if (descriptor) {
      assert.equal(descriptor.digest, info.platformManifest, 'container must execute the selected native manifest');
      assert.equal(descriptor.platform?.os, info.nativeOS, 'container native OS must match');
      assert.equal(descriptor.platform?.architecture, info.nativeArchitecture, 'container native architecture must match');
    } else {
      assert.ok(observed.Image === info.imageConfig || observed.Image === info.platformManifest,
        'index-level container identity requires its native manifest descriptor');
    }
  }
  assert.ok(allowed.includes(observed.Image), 'container must execute the published image without a derived image build');
}

function authenticatePulled(info, image, manifest, native) {
  assert.equal(native.os, info.Os, 'selected native OS must match pulled image');
  assert.equal(native.architecture, info.Architecture, 'selected native architecture must match pulled image');
  assert.deepEqual(info.RootFS.Layers, native.rootfs.diff_ids, 'pulled layers must match the selected immutable native config');
  // Docker's image-inspect API can materialize empty container/config fields
  // omitted from the registry config. Compare their values, not JSON omission.
  const defaults = { Hostname: '', Domainname: '', Image: '', AttachStdin: false,
    AttachStdout: false, AttachStderr: false, Tty: false, OpenStdin: false,
    StdinOnce: false, Entrypoint: null, Volumes: null, OnBuild: null };
  assert.deepEqual({ ...defaults, ...info.Config }, { ...defaults, ...native.config },
    'pulled configuration must match the selected immutable native config');
  if (info.Id !== manifest.config.digest) {
    assert.equal(info.Descriptor?.digest, image.split('@')[1], 'containerd image descriptor must match the requested immutable digest');
    assert.equal(info.Id, info.Descriptor.digest, 'containerd image identity must match its descriptor');
  }
}

function fixture(directory) {
  fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(path.join(directory, 'acceptance.pkg.kio'), 'package acceptance;\n\nbridge {\n  main\n}\n');
  fs.writeFileSync(path.join(directory, 'main.kio'), fixtureSource);
  fs.mkdirSync(path.join(directory, '.vscode'), { recursive: true });
  write(path.join(directory, '.vscode/settings.json'), { 'chat.disableAIFeatures': true });
}

function validate([image, refresh, server, editor, output, cpus, memory]) {
  assert.match(image, /^ghcr\.io\/jdevuyst\/kio-devcontainer@sha256:[a-f0-9]{64}$/);
  assert.match(refresh, /^[a-f0-9]{40}$/);
  assert.ok(fs.statSync(server).isFile(), 'official cached server archive required');
  assert.ok(fs.statSync(path.join(editor, 'bin/code')).isFile(), 'official cached Electron distribution required');
  assert.ok(path.isAbsolute(output) && !fs.existsSync(output), 'fresh absolute output directory required');
  assert.match(cpus, /^[1-9][0-9]*$/);
  assert.match(memory, /^[1-9][0-9]*[gm]$/);
  for (const item of [server, editor, output]) assert.ok(!/[\n\r,]/.test(item), 'Docker mount paths cannot contain newline/comma');
}

function imageSource(file, image, indexFile, receiptFile) {
  const info = read(file)[0];
  assert.ok(info.RepoDigests.includes(image), 'pulled image must retain requested registry digest');
  assert.equal(info.Os, 'linux');
  assert.equal(info.Architecture, process.arch === 'x64' ? 'amd64' : process.arch);
  const revision = info.Config.Labels['org.opencontainers.image.revision'];
  assert.match(revision, /^[a-f0-9]{40}$/);
  if (indexFile) {
    const index = read(indexFile);
    const manifestDigest = index.manifests ? index.manifests.find(item => item.platform?.os === info.Os && item.platform?.architecture === info.Architecture)?.digest : image.split('@')[1];
    assert.match(manifestDigest, /^sha256:[a-f0-9]{64}$/);
    const manifest = JSON.parse(command('docker', ['buildx', 'imagetools', 'inspect', `${image.split('@')[0]}@${manifestDigest}`, '--raw']));
    const configs = JSON.parse(command('docker', ['buildx', 'imagetools', 'inspect', `${image.split('@')[0]}@${manifestDigest}`, '--format', '{{json .Image}}']));
    const native = configs[`${info.Os}/${info.Architecture}`] || configs;
    authenticatePulled(info, image, manifest, native);
    write(receiptFile, { requestedImage: image, platformManifest: manifestDigest, imageConfig: manifest.config.digest, dockerImageId: info.Id,
      nativeOS: info.Os, nativeArchitecture: info.Architecture, imageSourceLabel: revision });
  }
  return revision;
}

function prepare([repo, checkout, dir, image, name, cpus, memory, server, editor, base, refresh]) {
  const jsonc = dependencies(repo)('jsonc-parser');
  const errors = [];
  const config = jsonc.parse(fs.readFileSync(path.join(checkout, '.devcontainer/devcontainer.json'), 'utf8'), errors);
  assert.deepEqual(errors, []);
  assert.equal(config.remoteUser, 'vscode');
  assert.equal(config.onCreateCommand, '.devcontainer/on-create.sh');
  assert.equal(config.updateContentCommand, '.devcontainer/update-content.sh');
  assert.equal(config.postAttachCommand, '.devcontainer/post-attach.sh');
  assert.equal(config.remoteEnv.KIO_BIN, '${containerWorkspaceFolder}/kio-rs/target/debug/kio');
  for (const ws of workspaces) assert.ok(fs.existsSync(path.join(checkout, ws, 'Cargo.toml')), ws);
  for (const npm of ['ci/infra/highlight-agreement-js', 'tools/vscode-kio']) {
    assert.ok(fs.existsSync(path.join(checkout, npm, 'package-lock.json')), npm);
  }
  config.image = image;
  config.updateRemoteUserUID = false; // Keep the published image, without a derived UID-adjustment build.
  config.workspaceMount = `type=bind,source=${checkout},target=${workspace}`;
  config.workspaceFolder = workspace;
  config.mounts = [
    `type=bind,source=${dir},target=${control}`,
    `type=bind,source=${path.resolve(server)},target=/acceptance-server.tar.gz,readonly`,
    `type=bind,source=${path.resolve(editor)},target=/acceptance-editor,readonly`,
  ];
  config.containerEnv.TMPDIR = `${control}/tmp`;
  config.runArgs = ['--name', name, '--label', `kio.acceptance=${name}`, '--cpus', cpus, '--memory', memory, '--shm-size', '512m', '--init'];
  write(path.join(dir, 'devcontainer.json'), config);
  write(path.join(dir, 'inputs.json'), { image, imageSourceCommit: base, refreshCommit: refresh, cpus, memory, shmBytes: 536870912,
    containerTemporaryRoot: `${control}/tmp`, temporaryStorage: 'bind mount under the supplied host output directory',
    coverage: ['native Linux lifecycle and installed extension in Electron'],
    gaps: ['Docker Desktop/macOS', 'Remote Containers desktop transport', 'Codespaces orchestration', 'browser remote UI', 'other architecture'] });
  fs.mkdirSync(path.join(dir, 'driver-extension'));
  write(path.join(dir, 'driver-extension/package.json'), { name: 'kio-acceptance-driver', publisher: 'kio-test', version: '0.0.0', main: './index.cjs', engines: { vscode: '^1.95.0' } });
  fs.writeFileSync(path.join(dir, 'driver-extension/index.cjs'), 'exports.activate = () => {};\n');
}

function hooks(file, updateOnly) {
  const log = fs.readFileSync(file, 'utf8');
  for (const ws of workspaces) {
    assert.ok(log.includes(`update-content: cargo build --tests in ${ws}`), `required build hook missing: ${ws}`);
    if (!updateOnly) assert.ok(log.includes(`on-create: cargo fetch in ${ws}`), `required creation hook missing: ${ws}`);
  }
  if (!updateOnly) for (const npm of ['ci/infra/highlight-agreement-js', 'tools/vscode-kio']) {
    assert.ok(log.includes(`on-create: npm ci in ${npm}`), `required npm hook missing: ${npm}`);
  }
  assert.ok(log.includes('refresh-kio: cargo build --bin kio'), 'compiler refresh hook missing');
}

function terminalCompiler(directory, login = false, options = {}) {
  // Image startup discovers checkout binaries only in interactive workspace shells.
  const output = command('/bin/bash', [login ? '-lic' : '-ic', 'printf "\\036"; command -v kio'],
    { cwd: directory, ...options });
  const parts = output.split('\x1e');
  assert.equal(parts.length, 2, 'interactive terminal must reach the compiler probe after startup');
  return parts[1].trim();
}

function tools(commit) {
  nonroot(process.getuid());
  assert.equal(command('id', ['-un']), 'vscode');
  assert.equal(process.env.KIO_BIN, `${workspace}/kio-rs/target/debug/kio`);
  assert.equal(process.env.CARGO_HOME, '/home/vscode/.cargo');
  const versions = {};
  for (const tool of ['mise', 'cargo', 'rustc', 'node', 'npm', 'python3', 'tree-sitter', 'shellcheck', 'sccache']) {
    versions[tool] = { path: command('sh', ['-c', 'command -v "$1"', 'probe', tool]), version: command(tool, ['--version']) };
  }
  const version = command(process.env.KIO_BIN, ['--version']);
  let releaseTag;
  try { releaseTag = command('git', ['-C', workspace, 'describe', '--tags', '--exact-match', '--match', 'releases/v*']); } catch {}
  assert.ok(version.includes(commit.slice(0, 8)) || (releaseTag && version === `kio ${releaseTag.slice('releases/v'.length)}`), `lifecycle compiler is stale: ${version}`);
  assert.ok(command(process.env.KIO_BIN, ['--help']).includes('lsp'));
  for (const ws of workspaces) fs.accessSync(`${workspace}/${ws}/target`, fs.constants.W_OK);
  fs.accessSync(process.env.CARGO_HOME, fs.constants.W_OK);
  const evidence = { uid: process.getuid(), versions, compiler: { path: process.env.KIO_BIN, version, sha256: hash(fs.readFileSync(process.env.KIO_BIN)) },
    terminalKio: terminalCompiler(workspace), loginTerminalKio: terminalCompiler(workspace, true), commit };
  assert.equal(evidence.terminalKio, process.env.KIO_BIN, 'clean-checkout terminal and editor compiler must resolve to the lifecycle binary');
  assert.equal(evidence.loginTerminalKio, process.env.KIO_BIN, 'login terminal and editor compiler must resolve to the lifecycle binary');
  const file = `${control}/tools-${commit}.json`;
  write(file, evidence);
  console.log(JSON.stringify(evidence));
}

function server() {
  nonroot(process.getuid());
  const destination = path.join(process.env.HOME, '.vscode-server/bin/acceptance');
  fs.mkdirSync(destination, { recursive: true });
  command('tar', ['xzf', '/acceptance-server.tar.gz', '-C', destination, '--strip-components=1']);
  const product = read(path.join(destination, 'product.json'));
  const editorProduct = read('/acceptance-editor/resources/app/product.json');
  assert.ok(product.commit && editorProduct.commit, 'official editor/server commit provenance required');
  assert.equal(product.commit, editorProduct.commit, 'cached editor and server must be the same official build');
  write(`${control}/editor-provenance.json`, { commit: product.commit, version: read('/acceptance-editor/resources/app/package.json').version,
    serverArchiveSha256: hash(fs.readFileSync('/acceptance-server.tar.gz')), electronSha256: hash(fs.readFileSync('/acceptance-editor/code')) });
}

async function zipFiles(file) {
  const yauzl = dependencies(workspace)('yauzl');
  return new Promise((resolve, reject) => {
    yauzl.open(file, { lazyEntries: true }, (error, zip) => {
      if (error) return reject(error);
      const entries = {};
      let manifest;
      zip.on('error', reject);
      zip.on('end', () => resolve({ members: entries, manifest }));
      zip.on('entry', entry => {
        if (!entry.fileName.startsWith('extension/') || entry.fileName.endsWith('/')) return zip.readEntry();
        const relative = entry.fileName.slice('extension/'.length);
        assert.ok(!relative.split('/').includes('..') && !path.isAbsolute(relative), 'safe VSIX member path');
        zip.openReadStream(entry, (error, stream) => {
          if (error) return reject(error);
          const chunks = [];
          stream.on('error', reject);
          stream.on('data', data => chunks.push(data));
          stream.on('end', () => {
            const bytes = Buffer.concat(chunks);
            if (relative === 'package.json') manifest = JSON.parse(bytes.toString('utf8'));
            else entries[relative] = hash(bytes);
            zip.readEntry();
          });
        });
      });
      zip.readEntry();
    });
  });
}

async function installedPayload() {
  const baked = '/opt/kio-extension/kio.vsix';
  const { members, manifest } = await zipFiles(baked);
  const extensionDir = path.join(process.env.HOME, '.vscode-server/extensions');
  const installed = fs.readdirSync(extensionDir).filter(name => name.startsWith(`${id}-`));
  assert.equal(installed.length, 1, 'exactly one Kio installation required');
  const directory = path.join(extensionDir, installed[0]);
  const pkg = read(path.join(directory, 'package.json'));
  assert.equal(`${pkg.publisher}.${pkg.name}`, id);
  authenticateManifest(pkg, manifest);
  assert.ok(members['dist/extension.js'] && members['parsers/tree-sitter-kio.wasm'], 'baked executable and grammar required');
  for (const [file, digest] of Object.entries(members)) {
    assert.equal(hash(fs.readFileSync(path.join(directory, file))), digest, `installed file differs from baked VSIX: ${file}`);
  }
  const info = { bakedSha256: hash(fs.readFileSync(baked)), version: pkg.version, extensionPath: directory, members, manifest };
  write(`${control}/installed.json`, info);
  return info;
}

function authenticateManifest(installed, baked) {
  const { __metadata, ...manifest } = installed;
  assert.deepEqual(manifest, baked, 'installed manifest must equal baked manifest except editor-owned __metadata');
}

function authenticateEditorManifest(runtime, baked) {
  // The editor enriches its API object; every field from the baked manifest
  // still has to match, while installedPayload authenticates the disk manifest.
  authenticateManifest(Object.fromEntries(Object.keys(baked).map(key => [key, runtime[key]])), baked);
}

function fixtureHover(hover) {
  assert.ok(hover.some(entry => entry.contents.some(content => content.value === '```kio\n. -> .\n```')),
    'fixture hover must report its unit-to-unit function type');
}

function fixtureCompletion(result) {
  return result?.items?.some(item => (typeof item.label === 'string' ? item.label : item.label.label) === 'identity');
}

function hoverEvidence(hover) {
  return hover.map(entry => ({ contents: entry.contents.map(content => typeof content === 'string' ? content : content.value),
    range: entry.range && { start: { line: entry.range.start.line, character: entry.range.start.character },
      end: { line: entry.range.end.line, character: entry.range.end.character } } }));
}

function missingExecutable(error, extensionPath) {
  const target = error.message.match(/Cannot find module ['"]([^'"]+)['"]/)?.[1];
  assert.equal(target, path.join(extensionPath, 'dist/extension.js'), 'real activation error must identify the omitted baked executable as its missing target');
}

async function editor(stage, missingSubject) {
  nonroot(process.getuid());
  const installed = missingSubject || await installedPayload();
  const serverCli = path.join(process.env.HOME, '.vscode-server/bin/acceptance/bin/code-server');
  if (!missingSubject) assert.ok(command(serverCli, ['--list-extensions']).split(/\r?\n/).includes(id), 'server CLI must confirm installation');
  const fixtureDirectory = `${control}/fixture`;
  fixture(fixtureDirectory);
  const testElectron = dependencies(workspace)('@vscode/test-electron');
  await testElectron.runTests({
    reuseMachineInstall: true,
    vscodeExecutablePath: '/acceptance-editor/code',
    extensionDevelopmentPath: `${control}/driver-extension`,
    extensionTestsPath: `${control}/driver.cjs`,
    launchArgs: [fixtureDirectory, '--extensions-dir', path.dirname(installed.extensionPath), '--user-data-dir', `${control}/profile`, '--no-sandbox', '--skip-welcome', '--skip-release-notes', '--disable-workspace-trust'],
    extensionTestsEnv: { KIO_ACCEPTANCE_STAGE: stage, KIO_ACCEPTANCE_EXPECTED_PATH: installed.extensionPath },
  });
  console.log(`installed extension editor checks passed (${stage})`);
}

async function waitFor(subject, label) {
  const deadline = Date.now() + 30000;
  for (;;) {
    const result = await subject();
    if (result) return result;
    assert.ok(Date.now() < deadline, `timed out waiting for ${label}`);
    await new Promise(resolve => setTimeout(resolve, 100));
  }
}

function runningCompiler(expected) {
  const parents = new Map();
  for (const pid of fs.readdirSync('/proc').filter(name => /^[0-9]+$/.test(name))) {
    try {
      const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
      parents.set(Number(pid), Number(stat.slice(stat.lastIndexOf(')') + 2).split(' ')[1]));
    } catch (error) { if (!['ENOENT', 'ESRCH', 'EACCES'].includes(error.code)) throw error; }
  }
  const subjects = [];
  for (const pid of parents.keys()) {
    let ancestor = parents.get(pid);
    const seen = new Set();
    while (ancestor && ancestor !== process.pid && !seen.has(ancestor)) { seen.add(ancestor); ancestor = parents.get(ancestor); }
    if (ancestor !== process.pid) continue;
    try {
      const args = fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0');
      if (!args.includes('lsp')) continue;
      const executable = fs.readlinkSync(`/proc/${pid}/exe`);
      const sha256 = hash(fs.readFileSync(`/proc/${pid}/exe`));
      assert.equal(executable, fs.realpathSync(expected.path), 'the editor must spawn the lifecycle-selected compiler');
      assert.equal(sha256, expected.sha256, 'the live LSP must execute the refreshed compiler bytes');
      subjects.push({ pid, executable, sha256 });
    } catch (error) { if (!['ENOENT', 'ESRCH', 'EACCES'].includes(error.code)) throw error; }
  }
  assert.equal(subjects.length, 1, 'one live real LSP descendant of this editor extension host required');
  return subjects[0];
}

exports.run = async function run() {
  const vscode = require('vscode');
  const extension = vscode.extensions.getExtension(id);
  assert.ok(extension, 'installed Kio extension must be discoverable');
  assert.equal(extension.id, id);
  assert.equal(extension.extensionPath, process.env.KIO_ACCEPTANCE_EXPECTED_PATH);
  const installed = read(`${control}/installed.json`);
  if (process.env.KIO_ACCEPTANCE_STAGE === 'missing-asset') {
    await assert.rejects(() => extension.activate(), error => {
      missingExecutable(error, installed.extensionPath);
      write(`${control}/missing-asset-observed.json`, { editorActivationRejectedMissingExecutable: true, error: error.message });
      return true;
    });
    throw new Error('real editor rejected the omitted baked executable');
  }
  authenticateEditorManifest(extension.packageJSON, installed.manifest);
  for (const [file, digest] of Object.entries(installed.members)) {
    assert.equal(hash(fs.readFileSync(path.join(extension.extensionPath, file))), digest, `editor payload: ${file}`);
  }
  assert.equal(vscode.workspace.getConfiguration('kio').get('server.path'), null, 'use lifecycle KIO_BIN selection');
  const document = await vscode.workspace.openTextDocument(`${control}/fixture/main.kio`);
  assert.equal(document.languageId, 'kio');
  await vscode.window.showTextDocument(document);
  await extension.activate();
  assert.ok(extension.isActive);
  const tokens = await waitFor(() => vscode.commands.executeCommand('vscode.provideDocumentSemanticTokens', document.uri), 'baked highlighting');
  assert.ok(tokens.data.length >= 5, 'baked grammar must classify tokens');
  const position = document.positionAt(document.getText().lastIndexOf('identity') + 3);
  const hover = await waitFor(async () => {
    const result = await vscode.commands.executeCommand('vscode.executeHoverProvider', document.uri, position);
    return result?.length ? result : undefined;
  }, 'real hover');
  const completion = await waitFor(async () => {
    const result = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', document.uri, position);
    return fixtureCompletion(result) ? result : undefined;
  }, 'real completion');
  write(`${control}/semantic-${process.env.KIO_ACCEPTANCE_STAGE}.json`, { hover: hoverEvidence(hover), completion, tokenData: Array.from(tokens.data) });
  const edit = new vscode.WorkspaceEdit();
  const start = document.getText().lastIndexOf('identity(())');
  edit.replace(document.uri, new vscode.Range(document.positionAt(start), document.positionAt(start + 'identity(())'.length)), 'identity(identity)');
  assert.ok(await vscode.workspace.applyEdit(edit));
  await waitFor(() => vscode.languages.getDiagnostics(document.uri).some(diag => diag.source === 'kio' && diag.code === 14 && /type mismatch/.test(diag.message)), 'real type diagnostic');
  const diagnostics = vscode.languages.getDiagnostics(document.uri);
  const restore = new vscode.WorkspaceEdit();
  restore.replace(document.uri, new vscode.Range(document.positionAt(start), document.positionAt(start + 'identity(identity)'.length)), 'identity(())');
  assert.ok(await vscode.workspace.applyEdit(restore));
  await waitFor(() => vscode.languages.getDiagnostics(document.uri).every(diag => diag.source !== 'kio' || diag.severity !== vscode.DiagnosticSeverity.Error), 'diagnostic correction');
  // Collect the independent provider results before checking their contents.
  write(`${control}/semantic-${process.env.KIO_ACCEPTANCE_STAGE}.json`, { hover: hoverEvidence(hover), completion, diagnostics,
    correctedDiagnostics: vscode.languages.getDiagnostics(document.uri), tokenData: Array.from(tokens.data) });
  fixtureHover(hover);
  assert.ok(fixtureCompletion(completion), 'completion must contain the fixture binding');
  const inputs = read(`${control}/inputs.json`);
  const commit = ['refreshed', 'restored'].includes(process.env.KIO_ACCEPTANCE_STAGE) ? inputs.refreshCommit : inputs.imageSourceCommit;
  const liveCompiler = runningCompiler(read(`${control}/tools-${commit}.json`).compiler);
  write(`${control}/editor-${process.env.KIO_ACCEPTANCE_STAGE}.json`, { extensionPath: extension.extensionPath, version: extension.packageJSON.version,
    bakedSha256: installed.bakedSha256, liveCompiler, activation: true, highlighting: true, hover: true, completion: true, diagnostic: true, correction: true });
};

async function injection() {
  const info = await installedPayload();
  const executable = path.join(info.extensionPath, 'dist/extension.js');
  const hidden = path.join(process.env.HOME, '.vscode-server/acceptance-hidden-executable');
  const before = hash(fs.readFileSync(executable));
  fs.renameSync(executable, hidden);
  try {
    await assert.rejects(() => editor('missing-asset', info));
    assert.equal(read(`${control}/missing-asset-observed.json`).editorActivationRejectedMissingExecutable, true,
      'the real editor, not a preflight check, must reject the missing installed executable');
  } finally {
    fs.renameSync(hidden, executable);
    assert.equal(hash(fs.readFileSync(executable)), before, 'restore exact baked executable bytes');
  }
  await editor('restored');
  write(`${control}/missing-asset.json`, { removedInstalledAsset: true, realEditorFailureObserved: true, restoredSha256: before,
    restoredSemanticChecksPassed: true, restoredEditor: read(`${control}/editor-restored.json`) });
}

function resources(stage) {
  const observations = {};
  for (const name of ['memory.peak', 'memory.events', 'memory.max', 'cpu.max']) {
    try { observations[name] = fs.readFileSync(`/sys/fs/cgroup/${name}`, 'utf8').trim(); }
    catch { observations[name] = null; }
  }
  write(`${control}/resources-${stage}.json`, { cgroupV2: observations, missingPeakMeansUnmeasured: true });
}

function groupMembers(group) {
  return cp.execFileSync('ps', ['-eo', 'pid=,ppid=,pgid=,stat='], { encoding: 'utf8', timeout: 10000 })
    .trim().split('\n').map(line => line.trim().split(/\s+/))
    .filter(fields => Number(fields[2]) === group)
    .map(fields => ({ pid: Number(fields[0]), parent: Number(fields[1]), state: fields[3][0] }));
}

function groupAlive(group) {
  return groupMembers(group).some(member => !['Z', 'X'].includes(member.state));
}

async function execute([output, label, bin, ...args]) {
  assert.match(label, /^[a-z][a-z0-9-]*$/);
  const log = fs.openSync(path.join(output, `${label}.log`), 'w');
  const started = Date.now();
  const child = cp.spawn(bin, args, { stdio: ['ignore', log, log], detached: process.platform !== 'win32' });
  let timer;
  let killTimer;
  let interrupted = false;
  const terminate = signal => {
    if (!child.pid) return;
    try { process.kill(process.platform === 'win32' ? child.pid : -child.pid, signal); } catch (error) { if (error.code !== 'ESRCH') throw error; }
  };
  const interrupt = () => {
    interrupted = true;
    terminate('SIGTERM');
    if (!killTimer) killTimer = setTimeout(() => terminate('SIGKILL'), 10000);
  };
  process.on('SIGTERM', interrupt);
  process.on('SIGINT', interrupt);
  let status;
  let failure;
  let descendants;
  let cleanupError;
  try {
    const budget = label.endsWith('-editor') || label === 'injection' ? 3 * 60 * 1000 : 90 * 60 * 1000;
    timer = setTimeout(interrupt, budget);
    status = await new Promise((resolve, reject) => { child.on('error', reject); child.on('close', (code, signal) => resolve({ code, signal })); });
    if (process.platform !== 'win32' && child.pid) {
      const alive = () => groupAlive(child.pid);
      descendants = { before: groupMembers(child.pid) };
      if (alive()) {
        // The CLI may leave owned transports after its leader exits. Drain them,
        // without treating kill(0)'s zombie-only group as executable work.
        terminate('SIGTERM');
        killTimer ||= setTimeout(() => terminate('SIGKILL'), 10000);
        const deadline = Date.now() + 15000;
        while (alive() && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 100));
        if (alive()) throw new Error('phase left live owned processes after TERM/KILL drainage');
      }
      if (descendants) descendants.after = groupMembers(child.pid);
    }
  } catch (error) {
    failure = error;
    status ||= { code: null, signal: null, error: error.code || error.message };
  } finally {
    if (failure) {
      // Observation failure cannot waive ownership or cancel pending escalation.
      try { terminate('SIGKILL'); } catch (error) { cleanupError = error.message; }
    }
    clearTimeout(timer);
    clearTimeout(killTimer);
    process.removeListener('SIGTERM', interrupt);
    process.removeListener('SIGINT', interrupt);
    fs.closeSync(log);
  }
  fs.appendFileSync(path.join(output, 'phases.jsonl'), JSON.stringify({ phase: label, elapsedMs: Date.now() - started, interrupted, descendants, cleanupError, ...status }) + '\n');
  if (failure) throw failure;
  if (interrupted) throw new Error(`phase ${label} interrupted; see ${label}.log`);
  if (status.code !== 0) throw new Error(`phase ${label} failed (${JSON.stringify(status)}); see ${label}.log; SIGKILL alone does not prove OOM`);
}

function finish(output) {
  const dir = path.join(output, 'control');
  const inputs = read(path.join(dir, 'inputs.json'));
  const first = read(path.join(dir, `tools-${inputs.imageSourceCommit}.json`));
  const refreshed = read(path.join(dir, `tools-${inputs.refreshCommit}.json`));
  assert.notEqual(first.compiler.sha256, refreshed.compiler.sha256, 'source refresh must replace compiler bytes');
  const phases = fs.readFileSync(path.join(output, 'phases.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
  const stages = ['first', 'reconnect', 'refreshed'];
  write(path.join(output, 'receipt.json'), { schema: 'kio-devcontainer-acceptance-v1', ...inputs, image: read(path.join(dir, 'image.json'))[0],
    publishedProvenance: read(path.join(dir, 'provenance.json')), cliVersionOutput: fs.readFileSync(path.join(output, 'cli.log'), 'utf8').trim(),
    editor: read(path.join(dir, 'editor-provenance.json')), bakedVsix: read(path.join(dir, 'installed.json')), tools: [first, refreshed], phases,
    finalContainer: read(path.join(output, 'container-final.json'))[0],
    stages: stages.map(stage => ({ stage, editor: read(path.join(dir, `editor-${stage}.json`)), resources: read(path.join(dir, `resources-${stage}.json`)) })),
    injection: read(path.join(dir, 'missing-asset.json')) });
}

function boundedDocker(args) {
  const result = cp.spawnSync('docker', args, { encoding: 'utf8', timeout: 60000, killSignal: 'SIGKILL' });
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `cleanup Docker command failed (${result.signal || result.status})`);
}

async function selftest(compiler) {
  const revision = 'a'.repeat(40);
  const image = `ghcr.io/jdevuyst/kio-devcontainer@sha256:${'b'.repeat(64)}`;
  const scratch = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'kio-acceptance-selftest-'));
  try {
    if (process.platform === 'linux') {
      const checkout = path.join(scratch, 'terminal-checkout');
      const home = path.join(scratch, 'terminal-home');
      const compiler = path.join(checkout, 'kio-rs/target/debug/kio');
      fs.mkdirSync(path.dirname(compiler), { recursive: true });
      fs.mkdirSync(home);
      fs.writeFileSync(compiler, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
      command('git', ['init', '--quiet', checkout]);
      fs.writeFileSync(path.join(home, '.bashrc'),
        'printf "fixture terminal startup\\n"\n. "$KIO_TEST_ACTIVATION"\n[ "$KIO_TOOLCHAIN_IDENTITY" = fixture-selection ] || exit 98\n');
      fs.writeFileSync(path.join(home, '.bash_profile'), '. "$HOME/.bashrc"\n');
      const options = { env: { ...process.env, HOME: home, PATH: '/usr/bin:/bin',
        KIO_TOOLCHAIN_IDENTITY: 'fixture-selection',
        KIO_TEST_ACTIVATION: path.resolve(__dirname, '../../../.devcontainer/activate.sh') } };
      assert.throws(() => command('sh', ['-c', 'command -v kio'], { cwd: checkout, ...options }),
        error => error.status === 127, 'noninteractive execution does not activate checkout binaries');
      for (const login of [false, true]) {
        assert.equal(terminalCompiler(checkout, login, options), compiler,
          `real interactive terminal must activate the checkout (login=${login})`);
      }
      fs.unlinkSync(compiler);
      assert.throws(() => terminalCompiler(checkout, false, options), error => error.status === 1,
        'interactive terminal must fail when its lifecycle compiler is absent');
      fs.writeFileSync(path.join(home, '.bashrc'), 'exit 0\n');
      assert.throws(() => terminalCompiler(checkout, false, options), /must reach the compiler probe/);
      console.log('acceptance terminals: login/non-login startup, explicit selection, absent compiler and early shell exit checked');
    }
    const file = path.join(scratch, 'image.json');
    write(file, [{ RepoDigests: [image], Os: 'linux', Architecture: process.arch === 'x64' ? 'amd64' : process.arch,
      Config: { Labels: { 'org.opencontainers.image.revision': revision } } }]);
    assert.equal(imageSource(file, image), revision);
    assert.throws(() => imageSource(file, image.replace('b', 'c')), /registry digest/);
    assert.throws(() => validate(['latest', revision, '', '', '', '4', '12g']));
    const native = { os: 'linux', architecture: 'amd64', rootfs: { diff_ids: ['layer-one'] }, config: { User: 'root' } };
    const manifest = { config: { digest: 'config-digest' } };
    const legacy = { Id: 'config-digest', Os: 'linux', Architecture: 'amd64', RootFS: { Layers: ['layer-one'] }, Config: native.config };
    authenticatePulled(legacy, image, manifest, native);
    const expanded = { ...legacy, Config: { ...native.config, Hostname: '', Domainname: '', Image: '',
      AttachStdin: false, AttachStdout: false, AttachStderr: false, Tty: false,
      OpenStdin: false, StdinOnce: false, Entrypoint: null, Volumes: null, OnBuild: null } };
    authenticatePulled(expanded, image, manifest, native);
    for (const [key, value] of Object.entries({ Hostname: 'other', AttachStdin: true,
      Entrypoint: ['changed'], Volumes: { '/changed': {} }, OnBuild: ['changed'], Unknown: null })) {
      assert.throws(() => authenticatePulled({ ...expanded, Config: { ...expanded.Config, [key]: value } },
        image, manifest, native), /pulled configuration/);
    }
    const modern = { ...legacy, Id: image.split('@')[1], Descriptor: { digest: image.split('@')[1] } };
    authenticatePulled(modern, image, manifest, native);
    assert.throws(() => authenticatePulled({ ...modern, RootFS: { Layers: ['wrong-layer'] } }, image, manifest, native), /pulled layers/);
    assert.throws(() => authenticatePulled({ ...modern, Config: { User: 'changed' } }, image, manifest, native), /pulled configuration/);
    assert.throws(() => authenticatePulled({ ...modern, Descriptor: { digest: 'wrong-index' } }, image, manifest, native), /requested immutable digest/);
    assert.throws(() => authenticatePulled(modern, image, manifest, { ...native, architecture: 'arm64' }), /native architecture/);
    assert.throws(() => authenticatePulled(modern, image, manifest, { ...native, os: 'different-os' }), /native OS/);
    nonroot(1000);
    assert.throws(() => nonroot(0), /nonroot/);
    const container = path.join(scratch, 'container.json');
    const published = path.join(scratch, 'published.json');
    write(published, [{ Id: 'published-config' }]);
    write(container, [{ Image: 'derived-config' }]);
    assert.throws(() => containerImage(published, container), /published image/);
    write(container, [{ Image: 'published-config' }]);
    containerImage(published, container);
    const provenance = path.join(scratch, 'provenance.json');
    const nativeProvenance = { requestedImage: image, imageConfig: 'published-config', platformManifest: 'published-native-manifest', nativeOS: 'linux', nativeArchitecture: 'amd64' };
    write(provenance, nativeProvenance);
    write(container, [{ Image: 'published-native-manifest', Config: { Image: image } }]);
    containerImage(published, container, provenance);
    write(container, [{ Image: 'derived-config', Config: { Image: image } }]);
    assert.throws(() => containerImage(published, container, provenance), /published image/);
    write(container, [{ Image: 'published-native-manifest', Config: { Image: 'wrong-reference' } }]);
    assert.throws(() => containerImage(published, container, provenance), /exact published reference/);
    write(published, [{ Id: image.split('@')[1] }]);
    const modernContainer = { Image: image.split('@')[1], Config: { Image: image },
      ImageManifestDescriptor: { digest: nativeProvenance.platformManifest, platform: { os: 'linux', architecture: 'amd64' } } };
    write(container, [modernContainer]);
    containerImage(published, container, provenance);
    write(container, [{ ...modernContainer, ImageManifestDescriptor: { ...modernContainer.ImageManifestDescriptor, digest: 'wrong-manifest' } }]);
    assert.throws(() => containerImage(published, container, provenance), /selected native manifest/);
    write(container, [{ ...modernContainer, ImageManifestDescriptor: { ...modernContainer.ImageManifestDescriptor, platform: { os: 'linux', architecture: 'arm64' } } }]);
    assert.throws(() => containerImage(published, container, provenance), /native architecture/);
    write(container, [{ Image: modernContainer.Image, Config: modernContainer.Config }]);
    assert.throws(() => containerImage(published, container, provenance), /requires its native manifest descriptor/);
    authenticateManifest({ name: 'kio', __metadata: { installedTimestamp: 1 } }, { name: 'kio' });
    assert.throws(() => authenticateManifest({ name: 'kio', main: 'rebuilt.js' }, { name: 'kio', main: 'baked.js' }), /baked manifest/);
    authenticateEditorManifest({ name: 'kio', main: 'baked.js', extensionLocation: { scheme: 'file' }, isBuiltin: false }, { name: 'kio', main: 'baked.js' });
    assert.throws(() => authenticateEditorManifest({ name: 'kio', main: 'rebuilt.js', isBuiltin: false }, { name: 'kio', main: 'baked.js' }), /baked manifest/);
    assert.throws(() => authenticateEditorManifest({ name: 'kio' }, { name: 'kio', main: 'baked.js' }), /baked manifest/);
    fixtureHover([{ contents: [{ value: '```kio\n. -> .\n```' }] }]);
    assert.throws(() => fixtureHover([{ contents: [{ value: 'identity' }] }]), /unit-to-unit/);
    assert.throws(() => fixtureHover([{ contents: [{ value: '```kio\n.\n```' }] }]), /unit-to-unit/);
    assert.ok(fixtureCompletion({ items: [{ label: 'identity' }] }));
    assert.ok(!fixtureCompletion({ items: [{ label: 'unrelated' }] }));
    assert.ok(!fixtureCompletion({ items: [] }));
    const opaque = Object.defineProperty({}, 'value', { value: '```kio\n. -> .\n```' });
    assert.equal(JSON.stringify(opaque), '{}');
    assert.equal(hoverEvidence([{ contents: [opaque] }])[0].contents[0], '```kio\n. -> .\n```');
    assert.notEqual(hoverEvidence([{ contents: [{ value: 'wrong' }] }])[0].contents[0], '```kio\n. -> .\n```');
    missingExecutable(new Error("Cannot find module '/installed/dist/extension.js'"), '/installed');
    assert.throws(() => missingExecutable(new Error('unrelated startup failure'), '/installed'), /omitted baked executable/);
    assert.throws(() => missingExecutable(new Error('Cannot find module /other/dist/extension.js'), '/installed'), /omitted baked executable/);
    assert.throws(() => missingExecutable(new Error('Permission denied /installed/dist/extension.js'), '/installed'), /omitted baked executable/);
    assert.throws(() => missingExecutable(new Error("Cannot find module '/other/dependency.js'\nRequire stack:\n- /installed/dist/extension.js"), '/installed'), /omitted baked executable/);
    assert.throws(() => missingExecutable(new Error("Cannot find module '/installed/dist/extension.js-other'"), '/installed'), /omitted baked executable/);
    const hookLog = path.join(scratch, 'hooks.log');
    fs.writeFileSync(hookLog, workspaces.map(ws => `on-create: cargo fetch in ${ws}\nupdate-content: cargo build --tests in ${ws}`).join('\n') + '\non-create: npm ci in ci/infra/highlight-agreement-js\non-create: npm ci in tools/vscode-kio\nrefresh-kio: cargo build --bin kio\n');
    hooks(hookLog);
    fs.writeFileSync(hookLog, 'refresh-kio: cargo build --bin kio\n');
    assert.throws(() => hooks(hookLog), /required build hook missing/);
    await execute([scratch, 'success', process.execPath, '-e', 'console.log("owned child completed")']);
    await assert.rejects(() => execute([scratch, 'failure', process.execPath, '-e', 'process.exit(3)']), /phase failure failed/);
    await assert.rejects(() => execute([scratch, 'missing-command', path.join(scratch, 'not-a-command')]), /ENOENT/);
    const phases = fs.readFileSync(path.join(scratch, 'phases.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
    assert.deepEqual(phases.map(phase => phase.code), [0, 3, null]);
    assert.equal(phases[2].error, 'ENOENT');
    if (process.platform === 'linux') {
      const orphan = 'const cp=require("node:child_process");const child=cp.spawn(process.execPath,["-e","setInterval(()=>{},1000)"],{stdio:"ignore"});child.unref();';
      await execute([scratch, 'owned-orphan', process.execPath, '-e', orphan]);
      const drained = JSON.parse(fs.readFileSync(path.join(scratch, 'phases.jsonl'), 'utf8').trim().split('\n').at(-1));
      assert.ok(drained.descendants.before.some(member => !['Z', 'X'].includes(member.state)), 'real live orphan must exercise owned drainage');
      assert.ok(drained.descendants.after.every(member => ['Z', 'X'].includes(member.state)), 'no live orphan may remain');
      assert.equal(drained.code, 0);
      console.log('acceptance process control: real live orphan drained; remaining zombies are not executable work');
    }
    if (compiler) {
      const directory = path.join(scratch, 'fixture');
      fixture(directory);
      command(compiler, ['check'], { cwd: directory, stdio: 'pipe' });
      fs.writeFileSync(path.join(directory, 'main.kio'), fixtureSource.replace('identity(())', 'identity(identity)'));
      assert.throws(() => command(compiler, ['check'], { cwd: directory, stdio: 'pipe' }), error => error.status === 14);
      console.log('acceptance editor fixture: clean source accepted; injected type mismatch exits 14');
      if (process.platform === 'linux') {
        const child = cp.spawn(compiler, ['lsp'], { cwd: directory, stdio: ['pipe', 'ignore', 'pipe'] });
        const closed = new Promise(resolve => child.once('close', resolve));
        let stderr = '';
        child.stderr.on('data', bytes => { stderr += bytes; });
        try {
          await new Promise((resolve, reject) => { child.once('spawn', resolve); child.once('error', reject); });
          const expected = { path: compiler, sha256: hash(fs.readFileSync(compiler)) };
          const live = await waitFor(() => {
            assert.equal(child.exitCode, null, `LSP exited during identity observation: ${stderr}`);
            try { return runningCompiler(expected); }
            catch (error) { if (error.message.startsWith('one live real LSP')) return undefined; throw error; }
          }, 'live compiler identity');
          assert.equal(live.pid, child.pid);
          assert.throws(() => runningCompiler({ ...expected, sha256: 'stale-bytes' }), /refreshed compiler bytes/);
          assert.throws(() => runningCompiler({ ...expected, path: path.join(scratch, 'container.json') }), /lifecycle-selected compiler/);
          console.log('acceptance live LSP identity: actual child verified; stale path/bytes rejected');
        } finally {
          child.kill('SIGTERM');
          const killTimer = setTimeout(() => child.kill('SIGKILL'), 10000);
          await closed;
          clearTimeout(killTimer);
        }
      }
    }
    console.log('devcontainer-acceptance selftest: digest, mutable rejection, installed manifest, image identity, nonroot, omitted hooks and child status passed');
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
}

if (require.main === module) {
  const [mode, ...args] = process.argv.slice(2);
  Promise.resolve().then(async () => {
    switch (mode) {
      case 'validate': return validate(args);
      case 'image-source': return console.log(imageSource(...args));
      case 'container-image': return containerImage(...args);
      case 'ownership': return ownership(...args);
      case 'docker': return boundedDocker(args);
      case 'prepare': return prepare(args);
      case 'hooks': return hooks(...args);
      case 'tools': return tools(...args);
      case 'server': return server();
      case 'editor': return editor(...args);
      case 'injection': return injection();
      case 'resources': return resources(...args);
      case 'execute': return execute(args);
      case 'finish': return finish(...args);
      case 'selftest': return selftest(...args);
      default: throw new Error(`unknown acceptance driver command: ${mode}`);
    }
  }).catch(error => { console.error(error); process.exitCode = 1; });
}
