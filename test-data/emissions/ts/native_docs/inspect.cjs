const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

if (['--strip-source', '--edit-source'].includes(process.argv[2])) {
  function strip(directory) {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory() && entry.name !== 'out') strip(file);
      else if (entry.isFile() && entry.name.endsWith('.kio')) {
        const text = fs.readFileSync(file, 'utf8');
        fs.writeFileSync(file, process.argv[2] === '--edit-source' ? text.replaceAll('EXPORT-DOC', 'EXPORT-EDIT-DOC') : text.split('\n').filter(line => !line.trimStart().startsWith('///')).join('\n'));
      }
    }
  }
  strip(process.argv[3]);
  process.exit(0);
}
const file = path.resolve(process.argv[2]);
const compiler = fs.realpathSync(process.argv[3]);
const ts = require(path.resolve(path.dirname(compiler), '../lib/typescript.js'));
const source = fs.readFileSync(file, 'utf8');
const exportMarker = process.argv[4] === '--edited' ? 'EXPORT-EDIT-DOC' : 'EXPORT-DOC';
const markers = ['HOST-FN-DOC', exportMarker, 'FIRST-SAME-DOC', 'SECOND-SAME-DOC', 'NEWTYPE-DOC', 'OPAQUE-DOC', 'GENERIC-DOC', 'EXISTENTIAL-DOC'];
markers.push('STRUCTURAL-DOC');
const has = (text, marker) => new RegExp('(^|[^A-Z-])' + marker + '([^A-Z-]|$)').test(text);
const syntax = text => ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, file.endsWith('.js') ? ts.ScriptKind.JS : ts.ScriptKind.TS);
if (process.argv[5] === '--absent') {
  for (const marker of [...markers, 'EXPORT-EDIT-DOC']) assert(!has(source, marker), 'cached stale documentation: ' + marker);
  function declarationTree(node, root) {
    const children = [];
    ts.forEachChild(node, child => { children.push(declarationTree(child, root)); });
    return [node.kind, children.length ? children : node.getText(root)];
  }
  const current = syntax(source), previous = syntax(fs.readFileSync(process.argv[4], 'utf8'));
  assert.deepEqual(declarationTree(current, current), declarationTree(previous, previous), 'documentation changed executable declarations');
  process.exit(0);
}
const program = ts.createProgram([file], { allowJs: true, noEmit: true, target: ts.ScriptTarget.ES2020 });
assert.equal(program.getSyntacticDiagnostics().length, 0);
const checker = program.getTypeChecker();
const root = program.getSourceFile(file);
const docs = symbol => ts.displayPartsToString(symbol.getDocumentationComment(checker));
function property(type, name) {
  const symbol = type.getProperty(name);
  assert(symbol, 'missing public property ' + name);
  return [symbol, checker.getTypeOfSymbolAtLocation(symbol, symbol.valueDeclaration || symbol.declarations[0])];
}
const candidates = [];
function walk(node) {
  if ((file.endsWith('.js') && ts.isObjectLiteralExpression(node)) || (!file.endsWith('.js') && ts.isInterfaceDeclaration(node))) {
    const type = checker.getTypeAtLocation(node), api = type.getProperty('testapi');
    if (api) {
      const nested = checker.getTypeOfSymbolAtLocation(api, api.valueDeclaration || api.declarations[0]);
      if (nested.getProperty('KioModule_main') && nested.getProperty('KioModule_other')) candidates.push(type);
    }
  }
  ts.forEachChild(node, walk);
}
walk(root);
const recursive = root.statements.filter(node => ts.isInterfaceDeclaration(node) && checker.getTypeAtLocation(node).getProperty('Root'));
assert.equal(recursive.length, 1, 'one actual recursive structural interface');
assert(has(docs(checker.getSymbolAtLocation(recursive[0].name)), 'STRUCTURAL-DOC'), 'recursive interface lost its declaration documentation');
assert.equal(candidates.length, 1, 'one actual public facade record');
function member(names) {
  let type = candidates[0], symbol;
  for (const name of names) [symbol, type] = property(type, name);
  return symbol;
}
for (const [names, yes, no] of [
  [['testapi', 'KioModule_main', 'same'], 'FIRST-SAME-DOC', 'SECOND-SAME-DOC'],
  [['testapi', 'KioModule_other', 'same'], 'SECOND-SAME-DOC', 'FIRST-SAME-DOC'],
  [['testapi', 'KioModule_main', 'main'], exportMarker, 'PRIVATE-DOC'],
]) {
  const symbol = member(names);
  assert(has(docs(symbol), yes), names.join('.') + ': missing ' + yes);
  assert(!has(docs(symbol), no), names.join('.') + ': wrong identity ' + no);
  assert.deepEqual(symbol.getJsDocTags(checker), [], 'authored prose became native metadata');
}
const mainDoc = docs(member(['testapi', 'KioModule_main', 'main']));
for (const [name, marker] of [['Token', 'NEWTYPE-DOC'], ['Opaque', 'OPAQUE-DOC'], ['Wrapper', 'GENERIC-DOC'], ['Packed', 'EXISTENTIAL-DOC']]) {
  const symbol = member(['testapi', 'KioType_' + name]);
  assert(has(docs(symbol), marker), name + ': documentation attached to wrong nominal namespace');
  assert.deepEqual(symbol.getJsDocTags(checker), []);
}
assert(!has(mainDoc, exportMarker === 'EXPORT-DOC' ? 'EXPORT-EDIT-DOC' : 'EXPORT-DOC'), 'stale edited documentation');
assert(mainDoc.includes('**bold**') && mainDoc.includes('*emphasis*') && mainDoc.includes('[guide](https://example.invalid/guide)'));
const tick = String.fromCharCode(96);
assert(mainDoc.includes('[' + tick + 'literal_code' + tick + ']') && mainDoc.includes('[' + tick + 'tilde_code' + tick + ']'), 'literal code shielding');
for (const marker of markers) assert(has(source, marker), 'missing ' + marker);
for (const marker of ['PRIVATE-DOC', 'MODULE-DOC', 'UNBRIDGED-DOC']) assert(!has(source, marker), 'leaked ' + marker);
if (file.endsWith('.js')) {
  const factory = root.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'createNativeDocs');
  assert(factory);
  const symbol = checker.getSymbolAtLocation(factory.name);
  assert(has(docs(symbol), 'HOST-FN-DOC') && docs(symbol).includes('host.testapi_io.print'));
  assert.deepEqual(symbol.getJsDocTags(checker), []);
}
