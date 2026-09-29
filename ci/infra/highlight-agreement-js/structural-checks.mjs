import assert from "node:assert/strict";
import { Parser, Query } from "web-tree-sitter";

const POSITIVE_SCOPE = /^(keyword|entity\.name|variable|comment|string|constant|support|punctuation)\./;

export function checkStructuralHighlighting(parser, grammar, fixtures, querySource, observe, classify) {
  const query = new Query(parser.language, querySource);
  const failures = [];
  const neutralLines = [];
  let targets = 0;
  let classifierTargets = 0;
  let classifierContextRoots = 0;
  let textmateTargets = 0;
  try {
    if (classify) {
      for (const [source, type, parentType] of [
        ["module source; fn run[*F]() { value }", "ambiguous_lbracket_star", "type_param_group"],
      ]) {
        parser.reset();
        const tree = parser.parse(source);
        try {
          const roots = tree.rootNode.descendantsOfType(type).filter((node) => node.parent?.type === parentType);
          assert.equal(roots.length, 1, `contextual classifier root: ${source}`);
          const node = roots[0];
          const inherited = classify(node.parent, source).filter(({ node: leaf }) =>
            leaf.startIndex >= node.startIndex && leaf.endIndex <= node.endIndex);
          const tokens = (leaves) => leaves.map(({ node: leaf, kind }) =>
            ({ kind, start: leaf.startIndex, end: leaf.endIndex }));
          assert.deepEqual(tokens(classify(node, source)), tokens(inherited),
            `subtree classifier retains parent context: ${source}`);
          classifierContextRoots++;
        } finally {
          tree.delete();
        }
      }
    }
    for (const fixture of fixtures) {
      parser.reset();
      const tree = parser.parse(fixture.source);
      assert.ok(tree, fixture.name);
      try {
        if (!fixture.partial && tree.rootNode.hasError) {
          failures.push(`${fixture.name}: ERROR or MISSING node`);
        }
        if (fixture.error && !tree.rootNode.hasError) {
          failures.push(`${fixture.name}: expected ERROR or MISSING node`);
        }
        for (const type of fixture.absent ?? []) {
          const contains = (node) => node.type === type || node.children.some(contains);
          if (contains(tree.rootNode)) failures.push(`${fixture.name}: unexpected ${type}`);
        }
        const captures = query.captures(tree.rootNode);
        const copy = tree.copy();
        const copiedParser = new Parser();
        copiedParser.setLanguage(parser.language);
        const copied = copiedParser.parse(fixture.source, copy);
        try {
          assert.deepEqual(snapshot(copied, query), snapshot(tree, query),
            `${fixture.name}: copied tree in a new parser`);
        } finally {
          copied.delete();
          copy.delete();
          copiedParser.delete();
        }
        let state = null;
        let offset = 0;
        const lines = fixture.source.split("\n").map((line) => {
          const priorState = state?.toString() ?? null;
          const result = grammar.tokenizeLine(line, state);
          assert.ok(!result.stoppedEarly, `${fixture.name}: TextMate stopped early`);
          const row = {
            start: offset,
            end: offset + line.length,
            priorState,
            finalState: result.ruleStack.toString(),
            tokens: result.tokens.map((token) => ({
              start: offset + token.startIndex,
              end: offset + token.endIndex,
              scopes: token.scopes,
            })),
          };
          offset += line.length + 1;
          state = result.ruleStack;
          return row;
        });
        for (const target of fixture.targets) {
          targets++;
          const label = `${fixture.name}:${target.start} ${target.text}`;
          assert.equal(
            fixture.source.slice(target.start, target.end), target.text, label,
          );
          const actual = captures
            .filter(({ node }) => node.startIndex < target.end && target.start < node.endIndex)
            .map(({ name, node }) => ({
              kind: name, start: node.startIndex, end: node.endIndex,
            }));
          if (actual.length === 0 || actual.some((capture) =>
            capture.kind !== target.kind || capture.start !== target.start ||
            capture.end !== target.end)) {
            failures.push(`${label}: query ${JSON.stringify(actual)}`);
          }
          if (classify) {
            classifierTargets++;
            const classified = classify(
              tree.rootNode.namedDescendantForIndex(target.start, target.end), fixture.source,
            );
            const leaves = classified.filter(({ node }) =>
              node.startIndex < target.end && target.start < node.endIndex);
            if (leaves.length !== 1 || leaves[0].node.startIndex !== target.start ||
                leaves[0].node.endIndex !== target.end || leaves[0].kind !== target.kind) {
              failures.push(`${label}: classifier ${JSON.stringify(leaves.map(({ node, kind }) =>
                ({ kind, start: node.startIndex, end: node.endIndex })))}`);
            }
          }
          const owners = [];
          for (
            let node = tree.rootNode.descendantForIndex(target.start, target.end);
            node;
            node = node.parent
          ) {
            owners.push(node.type);
          }
          if (!target.owners.every((owner) => owners.includes(owner))) {
            failures.push(`${label}: owners ${JSON.stringify(owners)}`);
          }
          observe?.({ engine: "tree-sitter", fixture: fixture.name, target, actual, owners });
          if (target.textmate === undefined) continue;
          textmateTargets++;
          const line = lines.find((row) =>
            row.start <= target.start && target.start < row.end);
          assert.ok(line, label);
          const tokens = line.tokens.filter((token) =>
            token.start < target.end && target.start < token.end);
          observe?.({ engine: "textmate", fixture: fixture.name, target, actual: tokens, line });
          if (tokens.length === 0 || tokens.some((token) => {
            const positive = token.scopes.filter((scope) => POSITIVE_SCOPE.test(scope));
            return token.start !== target.start || token.end !== target.end ||
              (target.textmate.length === 0 ? positive.length !== 0 :
                positive.length === 0 || positive.some((scope) => !target.textmate.includes(scope)));
          })) {
            failures.push(`${label}: TextMate ${JSON.stringify(tokens)}`);
          }
          if (target.textmate.length === 0) {
            neutralLines.push({ prefix: fixture.source.slice(0, line.end), line });
          }
        }
      } finally {
        tree.delete();
      }
    }
  } finally {
    query.delete();
  }
  assert.equal(neutralLines.length, 2, "signature module continuation pair");
  assert.deepEqual(neutralLines[0], neutralLines[1], "same complete prefix and TextMate state");
  console.log(`structural-highlighting: ${fixtures.length} fixtures, ${targets} raw query targets, ${classifierTargets} classifier targets, ${classifierContextRoots} classifier context roots, ${textmateTargets} raw TextMate targets, ${failures.length} failures`);
  assert.deepEqual(failures, [], failures.join("\n"));
}

function snapshot(tree, query) {
  const shape = (node) => ({
    type: node.type, start: node.startIndex, end: node.endIndex,
    missing: node.isMissing, children: node.children.map(shape),
  });
  return {
    tree: shape(tree.rootNode),
    captures: query.captures(tree.rootNode).map(({ name, node }) => ({
      kind: name, start: node.startIndex, end: node.endIndex,
    })),
  };
}

export function checkIncrementalHighlighting(parser, querySource) {
  const body = (value) => `module source; fn run() { id(${value}) }`;
  const declaration = (head) => `module source; varop ${head} { foldl join empty; };\nfn after() { let(x) }`;
  const imported = (head) => `module source; import ops(varop ${head}, value);\nfn after() { let(x) }`;
  const longOpen = `[${"+".repeat(96)}`;
  const longClose = `${"+".repeat(96)}]`;
  const cases = [
    ["module source; op _ + _ { impl add }", "module source; op _ + _ { ; impl add; }"],
    ["module source; op _ + _ { ; impl add }", "module source; op _ + _ { ;; impl add }"],
    ["module source; elab make: . { captures(helper) impl run }", "module source; elab make: . { captures(helper); impl run }"],
    ["module source; newtype Box: . { constructor box }", "module source; newtype Box: . { ;; constructor box;; }"],
    ["package source; build { docs { md \"guide.md\" } target rust { out \"out\" } }", "package source; build { docs { md \"guide.md\" }; target rust { out \"out\" } }"],
    ["module source; rec(loop) { fn first() { () } fn second() { () } }", "module source; rec(loop) { fn first() { () }; fn second() { () } }"],
    ["module source; rec { newtype A: . {constructor a} newtype B: . {constructor b} }", "module source; rec { newtype A: . {constructor a}; newtype B: . {constructor b} }"],
    ["signature app v(1); v(1) { with { module api { host type T } } breaking { remove { api.old } } }", "signature app v(1); v(1) { with { module api { host type T } }; breaking { remove { api.old } } }"],
    ["signature app v(1); v(1) { breaking { add { module api { host type T; } } } }", "signature app v(1); v(1) { breaking { add { module api { host type T } } } }"],
    ["module source; fn first() { () } fn after() { () }", "module source; fn first() { () }; fn after() { () }"],
    ["module source; fn run() { let .(<A> (_: A, right: A)) = value; value }",
      "module source; fn run() { let .(<A> (__: A, right: A)) = value; value }"],
    ["module source; fn run() { let .(<A> (__: A, right: A)) = value; value }",
      "module source; fn run() { let .(<A> (___: A, right: A)) = value; value }"],
    ["module source; fn run() { let .((_: A, right: A)) = value; value }",
      "module source; fn run() { let .((__: A, right: A)) = value; value }"],
    ["module source; fn run() { let .((__: A, right: A)) = value; value }",
      "module source; fn run() { let .((___: A, right: A)) = value; value }"],
    ["module source; fn run() { let .(<A> (left, right)) = value; value }",
      "module source; fn run() { let .(<A> (left: A, right)) = value; value }"],
    ["module source; fn run() { let .(<A> ((left, right), tail: A)) = value; value }",
      "module source; fn run() { let .(<A> ((left: A, right), tail: A)) = value; value }"],
    ["module source; fn run() { let .(<A> (,(left: A, right))) = value; value }",
      "module source; fn run() { let .(<A> ((left: A, right))) = value; value }"],
    ["module source; fn run() { let .(whole: (left, right)) = value; value }",
      "module source; fn run() { let .(whole: (left: A, right)) = value; value }"],
    ["module source; fn run() { let .(_: (left: A, right)) = value; value }",
      "module source; fn run() { let .(whole: (left: A, right)) = value; value }"],
    ["module source; fn run() { let .(<A> (left: A, right: A)) = value; value }",
      "module source; fn run() { let .(<A> (left: A, right: A): A) = value; value }"],
    ["module source; fn run() { let .() = value; value }",
      "module source; fn run() { let .(_) = value; value }"],
    ["module probe; fn run() { .x. { x1 } fn neighbor() { () }","module probe; fn run() { .x. { x1 } fn neighbor() { () } }"],
    ["module probe; fn run() { .x. { x1; pub fn neighbor() { () }","module probe; fn run() { .x. { x1; pub fn neighbor() { () } } }"],
    ["module probe; fn run() { .x. { x1; pure fn neighbor() { () }","module probe; fn run() { .x. { x1; pure fn neighbor() { () } } }"],
    ["module probe; fn run() { .x. { x1; pub pure fn neighbor() { () }","module probe; fn run() { .x. { x1; pub pure fn neighbor() { () } } }"],
    ["module probe; fn run() { .x. { x1; pure pub(probe) fn neighbor() { () }","module probe; fn run() { .x. { x1; pure pub(probe) fn neighbor() { () } } }"],
    ["module probe; fn run() { .x. { x1; pub(probe // scope\n /detail) pure fn neighbor() { () }","module probe; fn run() { .x. { x1; pub(probe // scope\n /detail) pure fn neighbor() { () } } }"],
    ["module probe; fn run() { .x. { x1; fn // header\nneighbor() { () }","module probe; fn run() { .x. { x1; fn // header\nneighbor() { () } } }"],
    ["module probe; fn run() { .x. { x1; fn neighbor // parameters\n[A]() { () }","module probe; fn run() { .x. { x1; fn neighbor // parameters\n[A]() { () } } }"],
    ["module probe; fn run() { fn neighbor() { () }","module probe; fn run() { fn neighbor() { () } }"],
    ["module probe; fn run() { do chain { value; fn neighbor() { () }","module probe; fn run() { do chain { value; fn neighbor() { () } } }"],
    ["module probe; fn run() { [* value; fn neighbor() { () }","module probe; fn run() { [* value; fn neighbor() { () } }"],
    ["module probe; fn run() { fn neighbor() { [* value }","module probe; fn run() { fn neighbor() { [* value } }"],
    ["module probe; fn run() { .x. { x1 } fn neighbor() { () } fn last() { () }","module probe; fn run() { .x. { x1 } fn neighbor() { () } fn last() { () } }"],
    ["module probe; fn run() { fn(value); pub(value); pure(value)","module probe; fn run() { fn(value); pub(value); pure(value) }"],
    [body("[*F] F(.) -> F(.)"), body("[* F *]")],
    [body("[[ x, [[ y ]] ]]"), body("[[ x, [[ let(y) ]] ]]")],
    ["module source; fn run() { id([* let(x),", body("[* let(x) *]")],
    ["module source; fn run() { id([[ let(x),", body("[[ let(x) ]]")],
    [body("if(x)"), body("if(x) { let(y) } else { do { y } }")],
    [body("-1"), body("x-1")],
    [body("[[ x +// note\n, let(x) ]]"), body("[[ x +, let(x) ]]")],
    [body("[[ .x.// note\n{ x1 }, let(x) ]]"), body("[[ .x. { x1 }, let(x) ]]")],
    [body(".t"), body(".t. { t1 }")],
    [body(".x. { x1 }"), body(".x1. { x1 }")],
    [body(".x. { x1 }"), body(".x . { x1 }")],
    ["module probe; fn run() { .x. { x1 } fn neighbor() { () }",
      "module probe; fn run() { .x. { x1 } } fn neighbor() { () }"],
    ["module probe; fn run() { .x. { x1; pure pub(probe) fn neighbor() { () }",
      "module probe; fn run() { .x. { x1 } } pure pub(probe) fn neighbor() { () }"],
    ["module probe; fn run() { .x. { x1; fn // header\nneighbor() { () }",
      "module probe; fn run() { .x. { x1 } } fn // header\nneighbor() { () }"],
    ["T.member.<<r", "T.member.<<.x. { x1 }"],
    ["module probe; fn run() { let(f) <- x }", "module probe; fn run() { let .(f) = x; f }"],
    [body("rec(poly)"), body("rec(poly) again(x)")],
    [body("rec(value)"), body("rec(value) again(x)")],
    [body("rec(poly, poly)"), body("rec(poly, poly) again(x)")],
    ["rec(poly)", "rec(poly)\n newtype(())"],
    [body("rec((poly), cont)"), body("rec((poly), cont) again(x)")],
    [body("rec([* poly, poly *], cont)"), body("rec([* poly, poly *], cont) again(x)")],
    [body("rec([+ poly, [* poly, poly *] +], cont) again(x)"),
      body("rec([+ poly, [* poly, poly *] %], cont) again(x)")],
    [body("rec([* poly, poly, cont) again(x)"), body("rec([* poly, poly *], cont) again(x)")],
    [body("rec(poly, cont) again(x)"), body("rec(,,poly,,cont,,) again(x)")],
    [body("rec"), body("rec again(x)")],
    [body("rec.helper"), body("rec.helper(x)")],
    [body("rec"), body("rec.<x")],
    ["module source; fn run() { rec(", "module source; fn run() { rec(poly) }"],
    ["module source; fn run() { rec(poly }\nfn after() { rec(value) }",
      "module source; fn run() { rec(poly) }\nfn after() { rec(value) }"],
    ["module source; host type Text role(str);", "module source; host type Text role(strange);"],
    ["module source; host type Text { owned };", "module source; host type Text { otherwise };"],
    ["module source; fn run() { let(helper.rec, value) }",
      "module source; fn run() { let .(rec, value) = pair; () }"],
    ["import app\nfn neighbor() { let(x) }",
      "import app as helper;\nfn neighbor() { let(x) }"],
    ["op import variadic foldl foldr foldl1 foldr1 finalize", "import variadic\nfn neighbor() { () }"],
    ["module m;\nimport app\nfn f() { () }", "module m;\nfn f() { () }\nimport app\n"],
    ["module source; varop [+ +] { foldl join empty; finalize finish; };",
      "module source; varop [+ +] { finalize finish; foldl join empty; };"],
    ["module source; elab make: . { captures (helper); impl run; };",
      "module source; elab make: . { impl run; captures (helper); };"],
    ["package app; build {} bridge {}", "package app; bridge {} build {}"],
    ["package app; build { cache (); docs { md \"guide.md\"; }; target rust { out \"out\"; } }",
      "package app; build { target rust { out \"out\"; }; docs { md \"guide.md\"; }; cache (); }"],
    ["dependency dep; source { path \"dep\"; } rehost old to fresh;",
      "dependency dep; rehost old to fresh; source { path \"dep\"; }"],
    ["signature app v(1); v(1) { with { module api { host type T; } }; breaking { remove { api.old; } } }",
      "signature app v(1); v(1) { breaking { remove { api.old; } }; with { module api { host type T; } } }"],
    ["signature app v(1); v(1) { breaking { add { api.fresh; }; remove { api.old; } } }",
      "signature app v(1); v(1) { breaking { remove { api.old; }; add { api.fresh; } } }"],
    [declaration("[+ +]"), declaration("[+ %]")],
    [imported("[+ +]"), imported("[+ %]")],
    [declaration("[+// gap\n+]"), declaration("[+// gap\n%]")],
    [declaration(`${longOpen} ${longClose}`), declaration(`${longOpen} %${longClose.slice(1)}`)],
    ["module source; varop [+\nfn after() { let(x) }", declaration("[+ +]")],
    ["module source; fn broken() { id([* x }\nfn after() { let(x) }",
      "module source; fn broken() { id([* x *]) }\nfn after() { let(x) }"],
  ];
  const query = new Query(parser.language, querySource);
  const point = (source, index) => {
    const lines = source.slice(0, index).split("\n");
    return { row: lines.length - 1, column: lines.at(-1).length };
  };
  let count = 0;
  try {
    for (let depth = 1; depth <= 5; depth++) {
      const source = "module probe; fn run() { " + ".x. { ".repeat(depth - 1) + "value";
      parser.reset();
      const tree = parser.parse(source);
      try {
        const blocks = tree.rootNode.descendantsOfType("incomplete_expression_block");
        assert.equal(blocks.length, depth, "one EOF recovery per unclosed body");
        assert.equal(new Set(blocks.map((node) => node.startIndex)).size, depth,
          "each EOF recovery owns a distinct opener");
        assert.ok(blocks.every((node) => node.endIndex === source.length),
          "recovery terminates exactly at EOF");
        assert.equal(tree.rootNode.descendantsOfType("fn_decl_head").length, 1);
      } finally {
        tree.delete();
      }
    }
    for (const pair of cases) {
      for (const [before, after] of [pair, [...pair].reverse()]) {
        parser.reset();
        const old = parser.parse(before);
        let start = 0, oldEnd = before.length, newEnd = after.length;
        while (start < oldEnd && start < newEnd && before[start] === after[start]) start++;
        while (oldEnd > start && newEnd > start && before[oldEnd - 1] === after[newEnd - 1]) {
          oldEnd--; newEnd--;
        }
        old.edit({
          startIndex: start, oldEndIndex: oldEnd, newEndIndex: newEnd,
          startPosition: point(before, start),
          oldEndPosition: point(before, oldEnd), newEndPosition: point(after, newEnd),
        });
        const incremental = parser.parse(after, old);
        const freshParser = new Parser();
        freshParser.setLanguage(parser.language);
        const fresh = freshParser.parse(after);
        try {
          assert.deepEqual(snapshot(incremental, query), snapshot(fresh, query),
            `incremental edit ${count}: ${JSON.stringify([before, after])}`);
          count++;
        } finally {
          old.delete(); incremental.delete(); fresh.delete(); freshParser.delete();
        }
      }
    }
  } finally {
    query.delete();
  }
  console.log(`structural incremental highlighting: ${count} exact edit/fresh comparisons`);
}
