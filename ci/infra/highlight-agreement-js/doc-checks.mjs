import assert from "node:assert/strict";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { Language, Parser, Query, Edit } from "web-tree-sitter";
import { createRequire } from "node:module";
import oniguruma from "vscode-oniguruma";
import textmate from "vscode-textmate";
import { docBodies, projectDocNode, docCodeGaps } from "../../../tools/vscode-kio/src/doc-embedded.mts";

const fixtures = JSON.parse(fs.readFileSync(new URL("../../../tools/tree-sitter-kio/test/documentation.json", import.meta.url), "utf8"));
const queryPath = new URL("../../../tools/tree-sitter-kio/queries/highlights.scm", import.meta.url);
const styles = JSON.parse(fs.readFileSync(new URL("../../../tools/tree-sitter-kio/test/documentation-styling.json", import.meta.url), "utf8"));

// These original fixtures pin reference/directive and fence partitioning.
// Inline-code/Markdown roles have their own complete positive controls below.
function coreRoles(tokens, fixture) {
  return tokens.filter(({ kind }) => ["comment.doc.reference", "comment.doc.directive", "comment.doc.fence"].includes(kind) ||
    (kind === "comment.doc.code" && fixture.targets.some((target) => target.kind === kind)));
}

const docScopes = new Map([
  ["variable.other.kiodoc.kio", "comment.doc.reference"],
  ["keyword.control.kiodoc.kio", "comment.doc.directive"],
  ["keyword.control.fence.kiodoc.kio", "comment.doc.fence"],
  ["string.unquoted.code.kiodoc.kio", "comment.doc.code"],
]);

export function checkDocTextMate(grammar) {
  for (const fixture of fixtures) {
    let state = null, offset = 0;
    const actual = [];
    for (const line of fixture.source.split("\n")) {
      const result = grammar.tokenizeLine(line, state);
      assert.ok(!result.stoppedEarly, fixture.name);
      for (const token of result.tokens) {
        const kind = token.scopes.toReversed().map((scope) => docScopes.get(scope)).find(Boolean);
        if (kind) {
          const start = offset + token.startIndex, end = offset + Math.min(token.endIndex, line.length);
          if (start < end) actual.push({ kind, start, end, text: fixture.source.slice(start, end) });
        }
      }
      state = result.ruleStack; offset += line.length + 1;
    }
    let targets = fixture.targets.filter(({ text }) => !fixture.lineLocalNeutral?.includes(text));
    if (["doc_multiline_literal", "doc_unmatched_inline", "doc_paragraph_boundary"].includes(fixture.name)) {
      // An unmatched delimiter may close on a later line. TextMate conservatively
      // keeps the remainder of this doc run neutral; whole-run engines stay exact.
      targets = [];
    } else if (fixture.name === "doc_non_fences") {
      targets = targets.slice(0, 1); // the indented unmatched delimiter starts fallback
    } else if (fixture.name === "doc_unfinished_hidden") {
      targets = []; // hidden-fence status requires later-line closing evidence
    } else if (fixture.name === "doc_hidden_fences") {
      targets = targets.filter(({ kind }) => kind === "comment.doc.reference");
    }
    assert.deepEqual(coreRoles(actual, fixture), targets.map(({ kind, start, end, text }) => ({ kind, start, end, text })), `${fixture.name}: TextMate`);
  }
  console.log(`documentation-textmate: ${fixtures.length} actual-engine fixtures (explicit hidden/inline lookahead fallbacks)`);
}

function snapshot(tree, query) {
  return query.captures(tree.rootNode).map(({ name, node }) =>
    ({ kind: name, start: node.startIndex, end: node.endIndex, text: node.text }));
}

function position(source, offset) {
  const prefix = source.slice(0, offset).split("\n");
  return { row: prefix.length - 1, column: prefix.at(-1).length };
}

function assertStyledTargets(fixture, actual, engine) {
  for (const target of fixture.targets) {
    if (engine === "TextMate" && target.textmate === null) continue;
    const kind = engine === "TextMate" ? target.textmate ?? target.kind : target.kind;
    const start = fixture.source.indexOf(target.text);
    assert.ok(start >= 0, `${fixture.name}: target exists`);
    for (let index = start; index < start + target.text.length; index++) {
      assert.ok(actual.some((token) => token.start <= index && index < token.end && token.kind === kind),
        `${fixture.name}: ${engine} ${target.text} at ${index} must be ${kind}: ${JSON.stringify(actual.filter((token) => token.start <= index && index < token.end))}`);
    }
  }
}

export function checkDocStyling(parser, grammar) {
  const query = new Query(parser.language, fs.readFileSync(queryPath, "utf8"));
  try {
    for (const fixture of styles) {
      parser.reset();
      const tree = parser.parse(fixture.source);
      try {
        const actual = query.captures(tree.rootNode).filter(({ node }) => !["doc_kio_code", "doc_kio_self_code"].includes(node.type))
          .map(({ name, node }) => ({ kind: name, start: node.startIndex, end: node.endIndex }));
        const bodies = docBodies(tree.rootNode);
        if (fixture.bodies !== undefined) assert.equal(bodies.length, fixture.bodies, `${fixture.name}: unrelated fences stay separate`);
        for (const body of bodies) {
          const embedded = parser.parse(body.source);
          try {
            const covered = [];
            for (const { name, node } of query.captures(embedded.rootNode)) {
              for (const span of projectDocNode(body, node)) {
                assert.ok(body.rows.some((origin) => origin && origin.startIndex <= span.start && span.end <= origin.endIndex), `${fixture.name}: projection remains in authenticated payload`);
                actual.push({ ...span, kind: name.startsWith("comment.doc") ? "comment.doc" : name });
                covered.push(span);
              }
            }
            actual.push(...docCodeGaps(body, covered).map((span) => ({ ...span, kind: "comment.doc.code" })));
          } finally { embedded.delete(); }
        }
        assertStyledTargets(fixture, actual, "Tree-sitter");
      } finally { tree.delete(); }
    }
    if (grammar) {
      const scopes = new Map([...docScopes,
        ["comment.line.documentation.kio", "comment.doc"],
        ["markup.heading.kiodoc.kio", "comment.doc.heading"],
        ["markup.bold.kiodoc.kio", "comment.doc.strong"],
        ["markup.italic.kiodoc.kio", "comment.doc.emphasis"],
        ["markup.underline.link.kiodoc.kio", "comment.doc.link"],
        ["punctuation.definition.marker.kiodoc.kio", "comment.doc.marker"],
        ["keyword.other.attribute.kiodoc.kio", "comment.doc.attribute"],
        ["keyword.declaration.kio", "keyword.declaration"],
        ["entity.name.function.kio", "entity.name.function"],
        ["string.quoted.double.kio", "literal.string"],
      ]);
      for (const fixture of styles) {
        const actual = [];
        let state = null, offset = 0;
        for (const line of fixture.source.split("\n")) {
          const result = grammar.tokenizeLine(line, state);
          assert.ok(!result.stoppedEarly, fixture.name);
          for (const token of result.tokens) {
            const kind = token.scopes.toReversed().map((scope) => scopes.get(scope)).find(Boolean);
            if (kind) actual.push({ kind, start: offset + token.startIndex, end: offset + Math.min(token.endIndex, line.length) });
          }
          state = result.ruleStack; offset += line.length + 1;
        }
        assertStyledTargets(fixture, actual, "TextMate");
      }
    }
    console.log(`documentation-styling: ${styles.length} actual query/adapter${grammar ? "/TextMate" : ""} fixtures`);
  } finally { query.delete(); }
}

export function checkDocHighlighting(parser, querySource = fs.readFileSync(queryPath, "utf8")) {
  const query = new Query(parser.language, querySource);
  let edits = 0;
  try {
    for (const fixture of fixtures) {
      parser.reset();
      const tree = parser.parse(fixture.source);
      try {
        assert.equal(tree.rootNode.hasError, fixture.error ?? false, `${fixture.name}: ${tree.rootNode}`);
        const captures = snapshot(tree, query);
        const actual = coreRoles(captures, fixture);
        const expected = fixture.targets.filter(({ text }) => !fixture.lineLocalNeutral?.includes(text)).map(({ kind, start, end, text }) => ({ kind, start, end, text }));
        assert.deepEqual(actual, expected, fixture.name);
        for (const target of fixture.targets.filter(({ text }) => fixture.lineLocalNeutral?.includes(text))) {
          assert.ok(captures.some(({ kind, start, end }) => kind === "comment.doc.fallback" && start <= target.start && end >= target.end), `${fixture.name}: explicit fallback must cover ${target.text}`);
        }
        const copy = tree.copy();
        const restoredParser = new Parser();
        restoredParser.setLanguage(parser.language);
        const restored = restoredParser.parse(fixture.source, copy);
        assert.deepEqual(snapshot(restored, query), snapshot(tree, query), `${fixture.name}: restored scanner`);
        restored.delete(); copy.delete(); restoredParser.delete();
      } finally { tree.delete(); }
    }
    for (const width of [2, 7, 8, 15, 31, 257]) {
      const marker = "`".repeat(width);
      const source = `/// prose ${marker}x${marker}\` [\`hidden\`] ${marker} [\`live\`].\nmodule app;\n`;
      parser.reset();
      const tree = parser.parse(source);
      try {
        const actual = snapshot(tree, query).filter(({ kind }) => kind === "comment.doc.reference" || kind === "comment.doc.directive");
        const start = source.lastIndexOf("live");
        assert.deepEqual(actual, [{ kind: "comment.doc.reference", start, end: start + 4, text: "live" }], `maximal same-line code width ${width}`);
      } finally { tree.delete(); }
    }
    const changes = [
      ["/// <!--kio\n/// [`inside`]\n", (s) => s + "/// -->\n"],
      ["/// ```text\n/// [`inside`]\n/// ```\n/// [`after`]\n", (s) => s.replace("```text", "````text")],
      ["/// ```text\n/// [`inside`]\n/// ```\n/// [`after`]\n", (s) => s.replace("/// ```\n", "/// ``\n")],
      ["/// [`name`]\nmodule app;\n", (s) => s.replace("///", "//")],
      ["/// ```text\n/// [`inside`]\n", (s) => s.replace("text\n", "text\n\n")],
      ["/// [`@ty", (s) => s + "pe Item`]"],
      ["/// ``start\n/// [`inside`]`` [`after`]\n", (s) => s.replace("]``", "]`")],
      ["/// [ unfinished\n/// [`reference\n", (s) => s + "/// ](url)\n"],
      ["/// [ unfinished\n/// [`reference\n", (s) => s.replace("reference", "reference`].")],
      ["/// ``unmatched\n/// [`neutral`]\n", (s) => s.replace("unmatched", "matched``")],
      ["/// ``unmatched\n/// [`neutral`]\n", (s) => s.replace("unmatched\n", "unmatched\n\n")],
      ["/// **before [`ref`] after**\n", (s) => s.replace("after**", "after*")],
      ["/// **before\n/// after**\n", (s) => s.replace("before\n", "before\n/// \n")],
      ["/// ```kio {@}\n/// let value = ();\n/// value\n/// ```\n", (s) => s.replace("{@}", "{@named}")],
      ["/// ```kio\n/// fn text() { \"unfinished\n/// ```\n/// [`after`].\n", (s) => s.replace("unfinished", "closed\" }")],
    ];
    for (const [before, change] of changes) {
      const after = change(before);
      let offset = 0, suffix = 0;
      while (offset < before.length && offset < after.length && before[offset] === after[offset]) offset++;
      while (suffix < before.length - offset && suffix < after.length - offset && before.at(-suffix - 1) === after.at(-suffix - 1)) suffix++;
      const oldText = before.slice(offset, before.length - suffix);
      const newText = after.slice(offset, after.length - suffix);
      for (const [from, to, removed, inserted] of [[before, after, oldText, newText], [after, before, newText, oldText]]) {
        parser.reset();
        const old = parser.parse(from);
        old.edit(new Edit({ startIndex: offset, oldEndIndex: offset + removed.length, newEndIndex: offset + inserted.length,
          startPosition: position(from, offset), oldEndPosition: position(from, offset + removed.length), newEndPosition: position(to, offset + inserted.length) }));
        const incremental = parser.parse(to, old);
        const freshParser = new Parser(); freshParser.setLanguage(parser.language);
        const fresh = freshParser.parse(to);
        assert.deepEqual(snapshot(incremental, query), snapshot(fresh, query), `doc edit ${edits}: incremental captures`);
        assert.equal(incremental.rootNode.toString(), fresh.rootNode.toString(), `doc edit ${edits}: recovery tree`);
        old.delete(); incremental.delete(); fresh.delete(); freshParser.delete();
        edits++;
      }
    }
    console.log(`documentation-highlighting: ${fixtures.length} shipped-query fixtures, six maximal-width inline controls, ${edits} bidirectional edits`);
  } finally { query.delete(); }
}

export function checkDocScannerCosts(parser) {
  const subjects = ["ordinary prose", "[`reference`].", "<!--kio", "[".repeat(16), "``unfinished", "distinct widths", "mixed widths", "same-line closed", "same-line varying widths", "**bold** *emphasis*", "**outer __inner *single _deep_ single* inner__ outer**", "*unclosed prose"];
  for (const body of subjects) {
    let previous = 0, previousLength = 0;
    for (const lines of [64, 128, 256]) {
      let source = Array.from({ length: lines }, (_, index) => {
        if (body === "distinct widths") return `/// prose ${"`".repeat(index + 1)} unmatched\n`;
        if (body === "mixed widths") return `/// \`matched\` prose ${"`".repeat(index + 2)} unmatched [\`neutral\`]\n${index % 2 ? "\n" : ""}`;
        return `/// ${body}\n`;
      }).join("") + "module app;\n";
      if (body === "same-line closed") source = `/// ${"`x` ".repeat(lines)}\nmodule app;\n`;
      if (body === "same-line varying widths") source = `/// ${Array.from({ length: lines }, (_, index) => {
        const marker = "`".repeat(index % 5 + 1);
        return `${marker}x${marker} `;
      }).join("")} unmatched \`\` tail [\`neutral\`]\nmodule app;\n`;
      let external = false, advances = 0;
      parser.reset();
      parser.setLogger((message, lexical) => {
        if (!lexical && message.startsWith("lex_external ")) external = true;
        else if (!lexical && message.startsWith("lex_internal ")) external = false;
        if (external && lexical && /^(consume|skip) character:/.test(message)) advances++;
      });
      const tree = parser.parse(source);
      parser.setLogger(null);
      try { assert.ok(!tree.rootNode.hasError, `cost subject ${body}`); } finally { tree.delete(); }
      assert.ok(advances > 0, "logger must observe the real external scanner");
      assert.ok(advances <= 8 * source.length, `${body}: ${advances} scanner advances for ${source.length} characters`);
      if (previous) assert.ok(advances * previousLength <= 1.1 * previous * source.length, `${body}: work per character at ${lines}: ${previous} -> ${advances}`);
      previous = advances; previousLength = source.length;
    }
  }
  console.log(`documentation-scanner-cost: ${subjects.length} real-WASM subjects at64/128/256 units bounded by lexical advance counts`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  if (process.argv[2] === "--textmate") {
    const require = createRequire(import.meta.url);
    await oniguruma.loadWASM(fs.readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm")));
    const target = process.argv[3] ?? fileURLToPath(new URL("../../../tools/textmate-kio/kio.tmLanguage.json", import.meta.url));
    const registry = new textmate.Registry({
      onigLib: Promise.resolve({ createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns), createOnigString: (source) => new oniguruma.OnigString(source) }),
      loadGrammar: async () => textmate.parseRawGrammar(fs.readFileSync(target, "utf8"), target),
    });
    try { checkDocTextMate(await registry.loadGrammar("source.kio")); } finally { registry.dispose(); }
  } else {
    await Parser.init();
    const parser = new Parser();
    parser.setLanguage(await Language.load(process.argv[2]));
    try {
      checkDocHighlighting(parser, process.argv[3] ? fs.readFileSync(process.argv[3], "utf8") : undefined);
      checkDocScannerCosts(parser);
      checkDocStyling(parser);
    } finally { parser.delete(); }
  }
}
