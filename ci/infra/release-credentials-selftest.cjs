'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {cargoResult, toolResult, toolInvocation, check} = require('./release-credentials.cjs');
const {inspect} = require('./release-credentials-inventory.cjs');
const inventory = require('./release-credentials.json');
const witness = {errors: [{detail: 'this action can only be performed on the crates.io website'}]};
const unauthenticated = {errors: [{detail: 'this action requires authentication'}]};
const response = (status, body) => ({status, json: async () => body});
const root = path.resolve(__dirname, '../..');
const read = name => fs.readFileSync(path.join(root, name), 'utf8');

async function main() {
  assert.equal(cargoResult(403, witness), 'authenticated');
  for (const status of [200, 400, 404, 429, 500, 503]) assert.equal(cargoResult(status, witness), 'unknown');
  assert.equal(cargoResult(403, {errors: [...witness.errors, {detail: 'extra'}]}), 'unknown');
  assert.equal(cargoResult(403, {errors: [{detail: 'forbidden'}]}), 'unknown');
  assert.equal(cargoResult(403, {errors: [{detail: 'authentication failed'}]}), 'rejected');
  assert.equal(cargoResult(401, {}), 'rejected');
  assert.equal(toolResult(null, ''), 'authenticated');
  assert.equal(toolResult(new Error(), 'HTTP 401'), 'rejected');
  assert.equal(toolResult(new Error(), 'HTTP 503'), 'unknown');
  assert.equal(toolResult({killed: true}, 'HTTP 401'), 'unknown');
  assert.equal(toolResult({code: 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER'}, 'HTTP 403'), 'unknown');
  assert.equal(toolResult({code: 'ENOENT'}, 'HTTP 403'), 'unknown');
  assert.equal(toolResult(new Error(), 'Personal Access Token verification has failed'), 'unknown');

  const env = Object.fromEntries(inventory.map(entry => [entry.secret, `secret-value-${entry.secret}`]));
  for (const entry of inventory.filter(entry => entry.check !== 'cargo-token')) {
    const invocation = toolInvocation(entry, env[entry.secret], {...env, VSCE_MARKETPLACE_URL: 'https://wrong.invalid', OVSX_REGISTRY_URL: 'https://wrong.invalid', OVSX_USERNAME: 'other', OVSX_PASSWORD: 'other'});
    assert.equal(invocation.options.env.VSCE_MARKETPLACE_URL, 'https://marketplace.visualstudio.com');
    assert.equal(invocation.options.env.OVSX_REGISTRY_URL, 'https://open-vsx.org');
    assert(!invocation.options.env.OVSX_USERNAME && !invocation.options.env.OVSX_PASSWORD);
    assert.equal(invocation.options.env[entry.secret], env[entry.secret]);
    for (const other of inventory.filter(other => other.secret !== entry.secret)) assert(!invocation.options.env[other.secret]);
    assert(!invocation.args.join(' ').includes(env[entry.secret]), 'tokens never enter command arguments');
  }
  assert.throws(() => toolInvocation({check: 'unknown'}, 'token', env), /Unknown credential verifier/);
  const logs = [];
  let calls = 0;
  const fetcher = async (url, options) => {
    assert.equal(url, 'https://crates.io/api/v1/me');
    assert.equal(options.redirect, 'error');
    assert.equal(options.method, undefined, 'GET only');
    assert(!('Cookie' in options.headers), 'no authenticated browser session');
    if (calls++ === 0) {
      assert.equal(options.headers.Authorization, 'Bearer kio-preflight-invalid-control', 'negative control must be a known-invalid nonsecret token');
      return response(403, unauthenticated);
    }
    assert.equal(options.headers.Authorization, `Bearer ${env.CARGO_REGISTRY_TOKEN}`);
    return response(403, witness);
  };
  const verify = async (entry, token) => {assert.equal(token, env[entry.secret]); return 'authenticated';};
  assert.equal(await check({env, fetcher, verify, log: text => logs.push(text)}), 0);
  assert.equal(calls, 2);
  assert(logs.some(text => /publish scope.*unverified/.test(text)));
  assert(logs.some(text => /Reader role/.test(text)));
  for (const token of Object.values(env)) assert(!logs.join('\n').includes(token));
  assert.equal(await check({env: {}, fetcher: () => assert.fail('missing token makes no request'), verify, log: () => {}}), 1);
  assert.equal(await check({env, fetcher: async () => response(403, witness), verify, log: () => {}}), 1, 'unconditional cookie rejection is not authentication');
  assert.equal(await check({env, fetcher: async () => response(503, {}), verify, log: () => {}}), 1, 'negative control outages fail unknown');
  assert.equal(await check({env, fetcher: async () => {throw new Error(Object.values(env).join(' '));}, verify: async () => {throw new Error(Object.values(env).join(' '));}, log: text => logs.push(text)}), 1);
  for (const token of Object.values(env)) assert(!logs.join('\n').includes(token), 'exception details never leak credentials');
  for (const result of ['rejected', 'unknown']) {
    calls = 0;
    assert.equal(await check({env, fetcher, verify: async () => result, log: () => {}}), 1);
  }

  inspect(read, inventory);
  assert.throws(() => inspect(read, inventory.slice(1)), /differ/);
  assert.throws(() => inspect(name => name === '.github/workflows/pages.yml' ? read(name) + '\n    env:\n      PAGES_PAT: ${{ secrets.ADDED_PAGES_PAT }}\n' : read(name), inventory), /differ/, 'website publisher secrets also require coverage');
  assert.throws(() => inspect(read, [...inventory, {...inventory[0], secret: 'STALE_PAT'}]), /differ/);
  for (const secret of inventory) {
    assert.throws(() => inspect(name => name === '.github/workflows/publish-release.yml' ? read(name).replace(`secrets.${secret.secret}`, 'secrets.NEW_PUBLISH_PAT') : read(name), inventory), /differ/);
  }
  assert.throws(() => inspect(name => name.startsWith('.github/workflows/') && name !== '.github/workflows/release-credentials.yml' ? read(name).replaceAll('secrets.VSCE_PAT', 'secrets.GITHUB_TOKEN') : read(name), inventory), /differ/, 'removed publisher secret leaves a stale inventory entry');
  assert.throws(() => inspect(name => name === '.github/workflows/release-credentials.yml' ? read(name).replace('secrets.VSCE_PAT', 'secrets.OVSX_PAT') : read(name), inventory), /actual secret/);
  assert.throws(() => inspect(name => name === '.github/workflows/publish-crate.yml' ? read(name).replace('secrets.CARGO_REGISTRY_TOKEN', "secrets['CARGO_REGISTRY_TOKEN']") : read(name), inventory), /explicit inventory support/);
  assert.throws(() => inspect(read, inventory.map(entry => ({...entry, target: 'other-publisher'}))), /published manifest/);
  for (const reference of ['./.github/workflows/new-publisher.yaml', '"./.github/workflows/new.publisher.yml"', "'./.github/workflows/new.publisher.yaml'"]) {
    assert.throws(() => inspect(name => {
      if (name === '.github/workflows/publish-release.yml') return `${read(name)}\n  added:\n    uses: ${reference}\n    secrets: inherit\n`;
      if (/new[.-]publisher/.test(name)) return 'jobs:\n  publish:\n    env:\n      ADDED_PAT: ${{ secrets.ADDED_PAT }}\n';
      return read(name);
    }, inventory), /differ/, 'every local workflow syntax discovers added publisher secrets');
  }
  console.log('release credential selftest: authentication, negative controls, failure isolation, secret safety and inventory drift pass');
}
main().catch(error => {console.error(error); process.exitCode = 1;});
