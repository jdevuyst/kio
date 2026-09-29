const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const left = [
  'item-api_2fleft-make', 'item-api_2fleft-Alias', 'item-api_2fleft-text',
  'item-api_2fleft-Box', 'labels-api_2fleft-Row', 'item-api_2fleft-Field',
  'item-api_2fleft-Other', 'item-api_2fleft-Foo', 'item-api_2fleft-add',
  'item-api_2fleft-negate', 'item-api_2fleft-subtract',
  'op-api_2fleft-op_20_5f_20_2b_20_5f', 'op-api_2fleft-op_20_2d_20_5f_5f',
  'op-api_2fleft-op_20_5f_20_2d_20_5f_5f', 'item-api_2fleft-choose',
  'item-api_2fleft-Text', 'item-api_2fleft-emit', 'item-api_2fleft-first',
  'item-api_2fleft-Chain', 'item-api_2fleft-Node',
  'labels-api_2fleft-Markers', 'item-api_2fleft-Mark',
];
const right = ['item-api_2fright-make', 'item-api_2fright-Box', 'item-api_2fright-build_5fbox'];
const sectionIds = text => [...text.matchAll(/<h3 id="([^"]+)"/g)].map(match => match[1]);

function files(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const file = path.join(directory, entry.name);
    return entry.isDirectory() ? files(file) : [file];
  });
}

for (const [root, ext] of [['out/docs', '.html'], ['out/docs-md', '.md']]) {
  const boundary = fs.readFileSync(path.join(root, `package-boundary${ext}`), 'utf8');
  assert.deepEqual(sectionIds(boundary).sort(), [...left, ...right].sort());
  for (const [owner, expected] of [['left', left], ['right', right]]) {
    const module = fs.readFileSync(path.join(root, `api/${owner}${ext}`), 'utf8');
    for (const id of expected) assert.equal(sectionIds(module).filter(value => value === id).length, 1, id);
    const privateLink = `api/${owner}${ext}#item-api_2f${owner}-helper`;
    assert.equal(boundary.split(privateLink).length - 1, 1, privateLink);
  }
  for (const name of ['helper', 'foo', 'scoped', 'second', 'choose_5fimpl']) {
    assert(!sectionIds(boundary).includes(`item-api_2fleft-${name}`), name);
  }
  assert(!sectionIds(boundary).includes('item-outside-borrowed'));

  const intro = fs.readFileSync(path.join(root, `intro${ext}`), 'utf8');
  for (const name of ['Box', 'make']) {
    assert(intro.includes(`api/left${ext}#item-api_2fleft-${name}`), name);
    assert(!intro.includes(`api/right${ext}#item-api_2fright-${name}`), name);
  }
  assert(!intro.includes('#item-api_2fleft-foo'));
  assert(!intro.includes('#item-api_2fright-build_5fbox'));
  assert(!intro.includes('@signature'));
  assert(!intro.includes('@source'));
  assert(!intro.includes('@type'));

  const checkedLinks = [];
  for (const file of files(root).filter(file => file.endsWith(ext))) {
    const contents = fs.readFileSync(file, 'utf8');
    const ids = [...contents.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
    assert.equal(new Set(ids).size, ids.length, `duplicate id in ${file}`);
    const links = new Set([
      ...[...contents.matchAll(/href="([^"\s]*#[^"\s]+)"/g)].map(match => match[1]),
      ...[...contents.matchAll(/\]\(([^)\s]*#[^)\s]+)\)/g)].map(match => match[1]),
    ]);
    for (const link of links) {
      assert(!link.includes('://'), `unexpected external fragment ${link}`);
      const [relative, fragment] = link.split('#');
      const target = relative ? path.resolve(path.dirname(file), relative) : path.resolve(file);
      assert(target.startsWith(path.resolve(root) + path.sep), target);
      const targetText = fs.readFileSync(target, 'utf8');
      const targetIds = [...targetText.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
      assert.equal(targetIds.filter(id => id === fragment).length, 1, `${file}: ${link}`);
      checkedLinks.push([path.relative(root, file).split(path.sep).join('/'), link]);
    }
  }
  const expectedLinks = [
    [`api/left${ext}`, `../api/left${ext}#item-api_2fleft-helper`],
    [`api/right${ext}`, `../api/right${ext}#item-api_2fright-helper`],
    [`package-boundary${ext}`, `api/left${ext}#item-api_2fleft-helper`],
    [`package-boundary${ext}`, `api/right${ext}#item-api_2fright-helper`],
    [`intro${ext}`, `api/left${ext}#item-api_2fleft-Box`],
    [`intro${ext}`, `api/left${ext}#item-api_2fleft-make`],
  ];
  assert.deepEqual(checkedLinks.sort(), expectedLinks.sort(), `fragment links for ${ext}`);
}
console.log('public boundary sections and identity links agree in HTML and Markdown');
