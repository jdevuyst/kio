import assert from "node:assert/strict";
import { Edit, Query } from "web-tree-sitter";
import { docBodies, projectDocNode } from "../../../tools/vscode-kio/src/doc-embedded.mts";
import { macroHeads, macroScope } from "../../../tools/vscode-kio/src/macro-scope.mts";

export function checkMacroHighlighting(parser, grammar, querySource) {
  const source = `mod source;
import library {macro source as choose; macro x1; macro pack}
fn before() { later(()) }
macro later: . -> . { impl helper }
fn run(ordinary: .) {
 choose(()); choose(()); (choose)(()); ((choose))(());
 .(choose: . -> .) { choose(()) };
 choose(());
 .x. { x1(()) };
 x1(());
 let choose = choose(());
 choose(());
 .(pack: . -> .) { pack(()) };
 pack(());
 later(())
}
fn after() { choose(()) }
`;
  const tree = parser.parse(source);
  assert.ok(!tree.rootNode.hasError, tree.rootNode.toString());
  const heads = macroHeads(tree.rootNode);
  const positions = (word) => [...source.matchAll(new RegExp(`\\b${word}\\b`, "g"))].map((match) => match.index);
  const choose = positions("choose"), x1 = positions("x1"), pack = positions("pack"), later = positions("later");
  for (const position of [choose[0], choose[1], choose[2], choose[3], choose[4], choose[7], choose[9], choose[11], x1[0], x1[2], pack[0], pack[3], later[1], later[2]]) assert.ok(heads.has(position), `macro at ${position}: ${source.slice(position, position + 20)}`);
  for (const position of [choose[5], choose[6], choose[8], choose[10], x1[1], pack[1], pack[2], later[0]]) assert.ok(!heads.has(position), `ordinary at ${position}: ${source.slice(position, position + 20)}`);
  const query = new Query(parser.language, querySource);
  try {
    const captures = query.captures(tree.rootNode);
    for (const position of [choose[1], choose[2]]) assert.ok(captures.some(({ name, node }) => name === "entity.name.function" && node.startIndex === position), "query-only fallback retains callable role");
  } finally { query.delete(); }

  // Reuse unchanged later call subtrees while removing and restoring the tag.
  const start = source.indexOf("macro source");
  tree.edit(new Edit({ startIndex: start, oldEndIndex: start + 6, newEndIndex: start + 6,
    startPosition: { row: 1, column: start - source.indexOf("import library") },
    oldEndPosition: { row: 1, column: start - source.indexOf("import library") + 6 },
    newEndPosition: { row: 1, column: start - source.indexOf("import library") + 6 } }));
  const editedSource = source.slice(0, start) + "fn    " + source.slice(start + 6);
  const edited = parser.parse(editedSource, tree);
  try {
    assert.ok(!edited.rootNode.hasError, edited.rootNode.toString());
    const after = macroHeads(edited.rootNode);
    for (const position of [choose[1], choose[2], choose[3], choose[4], choose[7], choose[9], choose[11]]) assert.ok(!after.has(position), "tag edit retracts every later classification");
    const restored = parser.parse(source);
    try { assert.deepEqual(macroHeads(restored.rootNode), heads, "restoring import restores scope"); }
    finally { restored.delete(); }
  } finally { edited.delete(); tree.delete(); }

  for (const body of [
    "choose[A](value)[B](other)",
    "choose [A] (value) [B] (other)",
    "receiver.>choose[A](value)(other)",
    "choose(value)(other) { value } otherwise { other }",
  ]) {
    const stagedSource = `mod staged; import library {macro choose} fn run() { ${body} }`;
    const staged = parser.parse(stagedSource);
    try {
      assert.ok(!staged.rootNode.hasError, `staged macro: ${body}: ${staged.rootNode}`);
      assert.ok(macroHeads(staged.rootNode).has(stagedSource.indexOf("choose", stagedSource.indexOf("fn run"))), body);
    } finally { staged.delete(); }
  }

  for (const fragment of ["fn run() { choose(()) }", "mod other; fn run() { choose(()) }"]) {
    const isolated = parser.parse(fragment);
    try { assert.equal(macroHeads(isolated.rootNode).size, 0, "isolated contexts have no ambient macros"); }
    finally { isolated.delete(); }
  }
  for (const [body, marked, ordinary] of [
    [".x. { x1(()); .y. { x1(()) }; x1(()) }; x1(())", [".y. { x1", "}; x1(()) }; x1"], [".x. { x1"]],
    ["let .x. = x1(()); x1(())", ["= x1"], ["; x1"]],
    ["let .(field, choose) = choose(()); field(()); choose(())", ["= choose"], ["; choose"]],
  ]) {
    const localSource = `mod source; import library {macro x1; macro choose} fn run() { ${body} }`;
    const localTree = parser.parse(localSource);
    try {
      assert.ok(!localTree.rootNode.hasError, localTree.rootNode.toString());
      const classified = macroHeads(localTree.rootNode);
      for (const text of marked) {
        const at = localSource.indexOf(text) + text.lastIndexOf(text.includes("x1") ? "x1" : "choose");
        assert.ok(classified.has(at), `scoped macro ${text}: ${body}`);
      }
      for (const text of ordinary) {
        const at = localSource.indexOf(text) + text.lastIndexOf(text.includes("x1") ? "x1" : "choose");
        assert.ok(!classified.has(at), `scoped ordinary ${text}: ${body}`);
      }
    } finally { localTree.delete(); }
  }
  for (const body of [
    "a.f(()) + m { () }",
    "a . f(()) + m (value) { () }",
    "a.f(()) + m (value) { () }",
    "a.f(()) + m (.t) { () }",
    "a.f(()) + (m)(value) { () }",
    "a.f(()) + ((m))(value) { () }",
    "a.f(()) + r.>m (value) { () }",
    "a.f(()) + r.>((m))(value) { () }",
    "a.f[I32]( // argument\n ()) + m { () }",
    "a.f.other(()) + m { () }",
    "a.f(m(())) + m { () }",
    "a.f(()) + m (left + (right)) { () }",
    "a.f(()) + m (left + ()) { () }",
  ]) {
    const qualifiedSource = `mod source; import helpers as a; import helpers {macro m; macro q; op _ + _} fn run() { ${body} }`;
    const qualifiedTree = parser.parse(qualifiedSource);
    try {
      assert.ok(!qualifiedTree.rootNode.hasError, `qualified Bool member: ${body}`);
      const scope = macroScope(qualifiedTree.rootNode);
      const bodyStart = qualifiedSource.indexOf(body);
      const expectedHeads = [...body.matchAll(/\bm\b/g)].map((match) => bodyStart + match.index);
      assert.deepEqual([...scope.heads].filter((at) => at >= bodyStart), expectedHeads, `qualified alias and macro heads: ${body}`);
    } finally { qualifiedTree.delete(); }
  }
  for (const [body, selected] of [["let value = scoped { () }; value", false], ["let value { () }", false], ["let(value) { () }", true], ["(let)(value) { () }", true], ["let { () }", true], ["recur value { () }", false], ["recur(value) { () }", true]]) {
    const nativeSource = `mod source; import library {macro let; macro recur} fn run() { ${body} }`;
    const nativeTree = parser.parse(nativeSource);
    try {
      const at = nativeSource.lastIndexOf(body) + (body.startsWith("(") ? 1 : 0);
      assert.equal(macroHeads(nativeTree.rootNode).has(at), selected, `native structural ownership: ${body}`);
    } finally { nativeTree.delete(); }
  }
  for (const expression of ["(,m,)(())", "(m,)(())", "(,,(m),,,)(())", "m // call\n (())", "((m // group\n))(())", "value.>((m // UFCS\n))(())"]) {
    const commented = `mod source; import helpers {macro m} fn run() { ${expression} }`;
    const tree = parser.parse(commented);
    try {
      assert.ok(!tree.rootNode.hasError, expression);
      const at = commented.lastIndexOf(expression) + expression.indexOf("m");
      assert.ok(macroHeads(tree.rootNode).has(at), `commented transparent head: ${expression}`);
    } finally { tree.delete(); }
  }
  for (const receiver of ["value", "(value)", "(left, right)", "((left, right))", "(Foo.mk(left), Bar.mk(right))"]) {
    for (const expression of [`${receiver}.>call(())`, `${receiver}.>>call(())`, `call(()).<${receiver}`, `call(()).<<${receiver}`, `${receiver}.>((call))(())`]) {
      const source = `mod source; import helpers {macro call} fn run() { ${expression}; .(call: . -> .) { ${expression} } }`;
      const tree = parser.parse(source);
      try {
        assert.ok(!tree.rootNode.hasError, expression);
        const positions = [...source.matchAll(/\bcall\b/g)].map((match) => match.index);
        const heads = macroHeads(tree.rootNode);
        assert.ok(heads.has(positions[1]), `UFCS callee role independent of receiver: ${expression}`);
        assert.ok(!heads.has(positions[3]), `ordinary callee shadows UFCS macro: ${expression}`);
      } finally { tree.delete(); }
    }
  }
  const identifierSource = "mod source; import names {macro macro_name; macro as_received} fn assignment(as_fraction: . -> .) { as_received(()); as_fraction(()) }";
  const identifiers = parser.parse(identifierSource);
  try {
    assert.ok(!identifiers.rootNode.hasError, "contextual keyword prefixes stay whole identifiers");
    assert.ok(macroHeads(identifiers.rootNode).has(identifierSource.lastIndexOf("as_received")));
  } finally { identifiers.delete(); }
  for (const name of ["macro_value", "macro1", "macros"]) {
    for (const source of [
      `mod source; import names {macro source as ${name}} fn run() { ${name}(()) }`,
      `mod source; fn ${name}() { () } fn run() { ${name}(()) }`,
      `mod source; macro ${name}: . -> . { impl helper } fn run() { ${name}(()) }`,
    ]) {
      const tree = parser.parse(source);
      try { assert.ok(!tree.rootNode.hasError, `contextual macro keyword keeps whole name: ${name}`); }
      finally { tree.delete(); }
    }
  }
  const incompleteSelector = parser.parse("mod source; import names {macro as ");
  try { assert.equal(macroHeads(incompleteSelector.rootNode).size, 0, "synthetic close cannot settle macro-as selector"); }
  finally { incompleteSelector.delete(); }
  for (const name of ["x0", "x01", "x999999999999999999999999999999999999999"]) {
    const invalid = `mod source; import helper {macro ${name}} fn run() { .x. { x1; ${name}(()) } }`;
    const tree = parser.parse(invalid);
    try { assert.ok(!macroHeads(tree.rootNode).has(invalid.lastIndexOf(name)), `invalid numbered index retains local ownership: ${name}`); }
    finally { tree.delete(); }
  }
  for (const [selector, expected] of [["fn macro", false], ["fn macro as local", false], ["macro as", true], ["macro as as local", true], ["macro build;", true], ["macro as;", true]]) {
    const selected = parser.parse(`mod source; import provider {${selector}}`);
    try {
      assert.ok(!selected.rootNode.hasError, selector);
      assert.equal(selected.rootNode.descendantsOfType("import_macro_item").length > 0, expected, `complete contextual selector: ${selector}`);
    } finally { selected.delete(); }
  }
  const documented = parser.parse(`mod source;
/// \`\`\`kio {@}
/// later(())
/// \`\`\`
fn first() { () }
macro later: . -> . { impl helper }
`);
  try {
    const bodies = docBodies(documented.rootNode);
    assert.equal(bodies.length, 1);
    const example = parser.parse(bodies[0].source);
    try {
      const calls = example.rootNode.descendantsOfType("entity_name_function").filter((node) => macroHeads(example.rootNode).has(node.startIndex)).flatMap((node) => projectDocNode(bodies[0], node));
      assert.equal(calls.length, 1, "module-end documentation context classifies later macro");
      assert.equal(documented.rootNode.text.slice(calls[0].start, calls[0].end), "later", "only authored example bytes project");
    } finally { example.delete(); }
  } finally { documented.delete(); }
  let state = null;
  for (const line of source.split("\n")) {
    const result = grammar.tokenizeLine(line, state);
    state = result.ruleStack;
    if (line.includes("choose(()); choose")) {
      const calls = result.tokens.filter(({ startIndex }) => line.slice(startIndex).startsWith("choose"));
      assert.equal(calls.length, 4, "TextMate retains repeated and grouped call heads");
      for (const [index, token] of calls.entries()) assert.ok(token.scopes.includes(index < 2 ? "entity.name.function.kio" : "variable.other.kio"), `TextMate callable fallback: ${token.scopes}`);
    }
  }
  console.log("macro-highlighting: scoped, numbered, grouped, source-order, incremental tag edit and standalone fallback passed");
}
