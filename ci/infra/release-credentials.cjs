'use strict';
const {execFile} = require('node:child_process');
const path = require('node:path');
const inventory = require('./release-credentials.json');

// AuthCheck authenticates before rejecting token access to this cookie-only
// endpoint. Only its specific rejection witnesses authentication; a generic
// 403, or a public endpoint's 200, proves nothing about a supplied token.
// https://github.com/rust-lang/crates.io/blob/9d38693538751f292ef6d3ca479837b7c2145a21/src/auth.rs
// https://github.com/rust-lang/crates.io/blob/9d38693538751f292ef6d3ca479837b7c2145a21/src/controllers/user/me.rs
// Token lookup excludes revoked/expired tokens before that rejection:
// https://github.com/rust-lang/crates.io/blob/9d38693538751f292ef6d3ca479837b7c2145a21/crates/crates_io_database/src/models/token.rs
function cargoResult(status, body) {
  const details = body?.errors?.map(error => error.detail) || [];
  if (status === 403 && details.length === 1 && details[0] === 'this action can only be performed on the crates.io website') return 'authenticated';
  if (status === 401 || (status === 403 && details.includes('authentication failed'))) return 'rejected';
  return 'unknown';
}

function toolResult(error, output) {
  if (!error) return 'authenticated';
  if (error.killed || error.signal || error.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER' || typeof error.code === 'string') return 'unknown';
  if (/\b(401|403)\b|\bunauthorized\b|\bforbidden\b|\binvalid (?:personal access )?token\b|\btoken (?:is )?(?:invalid|expired|revoked|not found)\b/i.test(output)) return 'rejected';
  return 'unknown';
}

function toolInvocation(entry, token, environment) {
  const extension = path.resolve(__dirname, '../../tools/vscode-kio');
  const script = {vsce: '@vscode/vsce/vsce', ovsx: 'ovsx/bin/ovsx'}[entry.check];
  if (!script) throw new Error('Unknown credential verifier');
  const env = {...environment};
  for (const credential of inventory) delete env[credential.secret];
  env[entry.secret] = token;
  env.VSCE_MARKETPLACE_URL = 'https://marketplace.visualstudio.com';
  env.OVSX_REGISTRY_URL = 'https://open-vsx.org';
  delete env.OVSX_USERNAME;
  delete env.OVSX_PASSWORD;
  return {args: [path.join(extension, 'node_modules', script), 'verify-pat', entry.target], options: {
    cwd: extension, env, timeout: 45_000, killSignal: 'SIGKILL', maxBuffer: 64 * 1024,
  }};
}

async function verifyTool(entry, token, environment) {
  const {args, options} = toolInvocation(entry, token, environment);
  return new Promise(resolve => {
    execFile(process.execPath, args, options,
      (error, stdout, stderr) => resolve(toolResult(error, `${stdout}\n${stderr}`.split(token).join('[redacted]'))));
  });
}

async function verifyCargo(token, fetcher) {
  const options = {
    headers: {Authorization: 'Bearer kio-preflight-invalid-control', 'User-Agent': 'kio-release-credential-preflight (https://github.com/jdevuyst/kio)'},
    redirect: 'error', signal: AbortSignal.timeout(15_000),
  };
  const negative = await fetcher('https://crates.io/api/v1/me', options);
  const control = await negative.json();
  if (![401, 403].includes(negative.status) || cargoResult(negative.status, control) === 'authenticated') return 'unknown';
  const response = await fetcher('https://crates.io/api/v1/me', {
    ...options, headers: {...options.headers, Authorization: `Bearer ${token}`},
    signal: AbortSignal.timeout(15_000),
  });
  return cargoResult(response.status, await response.json());
}

async function check({env = process.env, fetcher = fetch, verify = verifyTool, log = console.log} = {}) {
  let failed = false;
  for (const entry of inventory) {
    const token = env[entry.secret];
    let result = 'missing';
    if (token) {
      try {
        result = entry.check === 'cargo-token' ? await verifyCargo(token, fetcher) : await verify(entry, token, env);
      } catch { result = 'unknown'; }
    }
    const label = `${entry.registry} (${entry.secret})`;
    if (result === 'authenticated') log(`${label}: authenticated. ${entry.limit}`);
    else {
      failed = true;
      log(`${label}: ${result === 'missing' ? 'credential missing' : result === 'rejected' ? 'credential or access rejected' : 'verification unavailable or response unsupported; credential validity unknown'}.`);
    }
  }
  log('This preflight does not publish anything or establish complete publication readiness. Review the stated scope/access limits before releasing.');
  return failed ? 1 : 0;
}

module.exports = {cargoResult, toolResult, toolInvocation, check};
if (require.main === module) check().then(code => {process.exitCode = code;}).catch(() => {
  console.error('Credential preflight failed; details suppressed to protect credentials.');
  process.exitCode = 1;
});
