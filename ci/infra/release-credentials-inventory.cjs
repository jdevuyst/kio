'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '../..');

function inspect(read, inventory) {
  const secrets = new Set();
  const visited = new Set();
  function visit(name) {
    if (visited.has(name) || name === '.github/workflows/release-credentials.yml') return;
    visited.add(name);
    const text = read(name);
    assert(!/secrets\s*\[/.test(text), `${name}: bracket/dynamic secret references need explicit inventory support`);
    for (const match of text.matchAll(/secrets\.([A-Za-z_][A-Za-z_0-9]*)/g)) {
      // Generated GITHUB_TOKEN is governed by workflow permissions, not a PAT.
      if (match[1] !== 'GITHUB_TOKEN') secrets.add(match[1]);
    }
    for (const match of text.matchAll(/uses:\s*([^\n]+)/g)) {
      const value = match[1].split(/\s+#/)[0].trim().replace(/^(['"])(.*)\1$/, '$2');
      if (!value.startsWith('./.github/workflows/')) continue;
      assert(/^\.\/.github\/workflows\/[A-Za-z0-9_.-]+\.ya?ml$/.test(value), `${name}: unrecognized local workflow reference`);
      visit(value.slice(2));
    }
  }
  visit('.github/workflows/release.yml');
  visit('.github/workflows/devcontainer-publish.yml');
  visit('.github/workflows/pages.yml');
  const names = inventory.map(entry => entry.secret);
  assert.equal(new Set(names).size, names.length, 'credential inventory has duplicates');
  assert.deepEqual([...secrets].sort(), [...names].sort(), 'publisher secrets and credential inventory differ');
  const preflight = read('.github/workflows/release-credentials.yml');
  const mappings = [...preflight.matchAll(/^\s+([A-Z_][A-Z_0-9]*): \$\{\{ secrets\.([A-Z_][A-Z_0-9]*) \}\}/gm)];
  assert.deepEqual(mappings.map(match => match[1]).sort(), [...names].sort(), 'preflight environment must cover exactly the inventory');
  assert(mappings.every(match => match[1] === match[2]), 'preflight must use each publisher\'s actual secret');
  const extension = JSON.parse(read('tools/vscode-kio/package.json'));
  const crate = /^name = "([^"]+)"$/m.exec(read('kio-rs/Cargo.toml'))?.[1];
  for (const entry of inventory) {
    assert(['cargo-token', 'vsce', 'ovsx'].includes(entry.check), 'every inventory entry needs an implemented verifier');
    assert.equal(entry.target, entry.check === 'cargo-token' ? crate : extension.publisher, 'credential target must match the published manifest');
    assert(entry.limit, 'verification limits must be disclosed');
  }
}

module.exports = {inspect};
if (require.main === module) {
  inspect(name => fs.readFileSync(path.join(root, name), 'utf8'), require('./release-credentials.json'));
  console.log('release credential inventory: publisher secrets, workflow mappings and targets agree');
}
