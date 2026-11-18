const assert = require('node:assert/strict');
const fs = require('node:fs');

const file = process.argv[2];
const compiler = fs.realpathSync(process.argv[3]);
const ts = require('node:module').createRequire(compiler)('typescript');
const source = ts.createSourceFile(file, fs.readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
assert.equal(source.parseDiagnostics.length, 0);
const declarations = new Map();
const hostCalls = new Map();
function visit(node, action) {
  action(node);
  ts.forEachChild(node, child => visit(child, action));
}
visit(source, node => {
  if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer) {
    const name = node.name.text;
    if (['inline_calls', 'inline_blocks', 'inline_operators', 'inline_effect', 'opaque', 'staged', 'not'].includes(name)) {
      assert(!declarations.has(name), `ambiguous subject ${name}`);
      declarations.set(name, node.initializer);
    }
    visit(node.initializer, child => {
      if (ts.isCallExpression(child) && ts.isPropertyAccessExpression(child.expression) &&
          ['condition', 'factory'].includes(child.expression.name.text)) {
        hostCalls.set(name, child.expression.name.text);
      }
    });
  }
});
function subject(name) {
  assert(declarations.has(name), `missing subject ${name}`);
  return declarations.get(name);
}
function unwrap(node) {
  while (ts.isParenthesizedExpression(node)) node = node.expression;
  return node;
}
function assertNoBranchFunction(name) {
  const root = unwrap(subject(name));
  assert(ts.isArrowFunction(root), `${name}: expected source function`);
  visit(root.body, node => {
    if (!ts.isArrowFunction(node) && !ts.isFunctionExpression(node)) return;
    let use = node;
    while (ts.isParenthesizedExpression(use.parent)) use = use.parent;
    assert(ts.isCallExpression(use.parent) && use.parent.expression === use,
      `${name}: a delayed branch function survived`);
  });
}
for (const name of ['inline_calls', 'inline_blocks', 'inline_operators']) {
  assertNoBranchFunction(name);
  const operators = new Set();
  visit(subject(name), node => {
    if (ts.isBinaryExpression(node)) operators.add(node.operatorToken.kind);
  });
  assert(operators.has(ts.SyntaxKind.AmpersandAmpersandToken), `${name}: missing native AND`);
  assert(operators.has(ts.SyntaxKind.BarBarToken), `${name}: missing native OR`);
}
assertNoBranchFunction('inline_effect');
let nativeNot = false;
visit(subject('not'), node => {
  if (ts.isPrefixUnaryExpression(node) && node.operator === ts.SyntaxKind.ExclamationToken) nativeNot = true;
});
assert(nativeNot, 'ordinary library NOT did not recover');
let opaqueCalls = 0;
visit(subject('opaque'), node => {
  if (ts.isCallExpression(node) && ts.isIdentifier(unwrap(node.expression)) && unwrap(node.expression).text === 'right') opaqueCalls++;
});
assert.equal(opaqueCalls, 1, 'the opaque RHS must retain one selected invocation');
const order = [];
visit(subject('staged'), node => {
  if (ts.isCallExpression(node)) {
    const callee = unwrap(node.expression);
    if (ts.isPropertyAccessExpression(callee) && ['condition', 'factory'].includes(callee.name.text)) order.push([callee.name.text, node.pos]);
    if (ts.isIdentifier(callee) && hostCalls.has(callee.text)) order.push([hostCalls.get(callee.text), node.pos]);
  }
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.AmpersandAmpersandToken) order.push(['dispatch', node.pos]);
});
order.sort((a, b) => a[1] - b[1]);
assert.deepEqual(order.map(([name]) => name), ['condition', 'factory', 'dispatch']);
