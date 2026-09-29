#!/usr/bin/env node
// Cross-tokenizer agreement check.
//
// For each fixture under test-data/highlight-corpus/, compare:
//   - kio debug tokens (the oracle, re-derived from the kio binary)
//   - tree-sitter WASM parser (exact token boundaries and kinds)
//   - vscode-textmate tokenization (positive roles must agree; neutrality
//     is limited to roles that require later-line evidence)
//
// Invoked with argv:
//   node check.mjs <corpus-dir> <tree-sitter-wasm-path> \
//                  <textmate-grammar-json> <kio-bin>

import assert from "node:assert/strict";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { checkStructuralHighlighting, checkIncrementalHighlighting } from "./structural-checks.mjs";
import { checkTextMateHighlighting } from "./textmate-checks.mjs";

// Both packages ship as CommonJS; ESM-import them through the
// default-export pattern.
import * as onigurumaMod from "vscode-oniguruma";
import * as vsctmMod from "vscode-textmate";
import { Language, Parser } from "web-tree-sitter";
const oniguruma = onigurumaMod.default ?? onigurumaMod;
const vsctm = vsctmMod.default ?? vsctmMod;

const execFileAsync = promisify(execFile);

const [, , corpusDir, tsWasmPath, tmGrammarPath, kioBin] = process.argv;
if (!corpusDir || !tsWasmPath || !tmGrammarPath || !kioBin) {
  console.error(
    "Usage: check.mjs <corpus-dir> <ts-wasm-path> <tm-grammar-json> <kio-bin>",
  );
  process.exit(2);
}

// ---- Tree-sitter side ----------------------------------------
//
// The shell wrapper regenerates the parser and builds a fresh WASM
// grammar before invoking this driver. Loading that grammar once
// keeps per-fixture checks independent without paying the
// tree-sitter CLI startup cost for every fixture.

// Map tree-sitter grammar node names → canonical TokenKind strings.
const TS_KIND_MAP = {
  comment_doc: "comment.doc",
  comment_line: "comment.line",
  elaborator_name: "keyword.elaborator",
  block_head: "keyword.elaborator",
  block_label: "keyword.control",
  keyword_control: "keyword.control",
  keyword_declaration: "keyword.declaration",
  keyword_dependency: "keyword.declaration",
  keyword_source: "keyword.declaration",
  keyword_lock: "keyword.declaration",
  keyword_resolved: "keyword.declaration",
  keyword_signature: "keyword.declaration",
  keyword_with: "keyword.declaration",
  keyword_breaking: "keyword.declaration",
  keyword_nonbreaking: "keyword.declaration",
  keyword_add: "keyword.declaration",
  keyword_modify: "keyword.declaration",
  keyword_remove: "keyword.declaration",
  // Structural-keyword tokens pulled out of keyword_declaration's
  // choice so the corresponding structural productions
  // (module_decl, import_decl) can match them. The split changes only the
  // tree-sitter node name; each entry keeps the reference classification.
  keyword_package: "keyword.declaration",
  keyword_module: "keyword.declaration",
  keyword_import: "identifier",
  keyword_op: "identifier",
  keyword_varop: "identifier",
  keyword_let: "keyword.declaration",
  keyword_as: "identifier",
  keyword_build: "keyword.declaration",
  keyword_host: "keyword.declaration",
  keyword_bridge: "keyword.declaration",
  // These are in the reference's classify_ident keyword table.
  keyword_type: "keyword.declaration",
  keyword_literal: "keyword.declaration",
  // `rec` remains an ordinary identifier outside the grammar positions that
  // introduce recursive data/groups/calls. `treeSitterKind` refines these
  // parser-confirmed occurrences; every other occurrence is an identifier.
  keyword_rec: "identifier",
  keyword_pub: "keyword.declaration",
  keyword_pure: "keyword.declaration",
  keyword_fn: "keyword.declaration",
  keyword_newtype: "keyword.declaration",
  keyword_labels: "keyword.declaration",
  keyword_equiv: "keyword.declaration",
  // Case-restricted IDENT tokens used in declaration-name slots.
  uppercase_identifier: "identifier",
  lowercase_identifier: "identifier",
  // Newtype member keywords are authenticated by their body owner.
  keyword_constructor: "keyword.declaration",
  keyword_projector: "keyword.declaration",
  bool_literal: "literal.bool",
  number_literal: "literal.number",
  string_literal: "literal.string",
  slot: "slot",
  operator_arrow_type: "operator.builtin",
  operator_arrow_ufcs: "operator.builtin",
  operator_row_suffix: "operator.user",
  operator_builtin: "operator.builtin",
  operator_run: "operator.user",
  unowned_symbol_run: "operator.user",
  // The star-shaped opener splits into bracket/kind leaves in a forall owner
  // and is emitted as one operator token in a variadic owner below.
  variadic_open: "operator.user",
  variadic_close: "operator.user",
  variadic_star_close: "operator.user",
  star_open_prefix: "punctuation.bracket",
  kind_annotation: "operator.user",
  // The `/` module-path separator. The canonical `kio debug tokens`
  // classifier paints `/` as `operator.user` (it is also the
  // user-definable division operator), so the dedicated structural
  // token maps to the same kind.
  module_path_sep: "operator.user",
  // Bracket tokens as named leaves so structural productions can
  // pin a specific character. The grammar has no catch-all
  // `$.bracket` rule; each character has its own dedicated token.
  bracket_lparen: "punctuation.bracket",
  bracket_rparen: "punctuation.bracket",
  bracket_lbrace: "punctuation.bracket",
  bracket_rbrace: "punctuation.bracket",
  bracket_lbracket: "punctuation.bracket",
  bracket_rbracket: "punctuation.bracket",
  // Punctuation separators as named leaves so structural
  // productions can require a specific character. The grammar
  // has no catch-all `$.separator` rule; each character has its
  // own dedicated token.
  separator_semicolon: "punctuation.separator",
  separator_comma: "punctuation.separator",
  separator_dot: "punctuation.separator",
  identifier: "identifier",
  // Module-path segments — emitted by `module_decl` and `import_decl`
  // around slash-separated provider paths.
  entity_name_module: "entity.name.module",
  // Type definition names — emitted by `newtype_decl_head` and the
  // named form of `labels_decl_head`.
  entity_name_type: "entity.name.type",
  // Function definitions and callees share the same role.
  entity_name_function: "entity.name.function",
  // Parameter binders inside function, lambda and forall signatures.
  variable_parameter: "variable.parameter",
  // Label entry names inside `labels { … }` blocks — emitted by
  // `label_entry`.
  entity_name_label: "entity.name.label",
};

const TREE_SITTER_NODE_KIND = /^[A-Za-z_][A-Za-z0-9_]*$/;

async function loadTreeSitterParser() {
  await Parser.init({
    locateFile(name) {
      if (name === "web-tree-sitter.wasm") {
        return fileURLToPath(
          import.meta.resolve("web-tree-sitter/web-tree-sitter.wasm"),
        );
      }
      return name;
    },
  });
  const language = await Language.load(tsWasmPath);
  const parser = new Parser();
  parser.setLanguage(language);
  return parser;
}

const EXACT_TYPE_NAME = /^_*[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*$/;

function treeSitterLeaves(root, source) {
  const leaves = [];
  const stack = [{ node: root, parentType: root.parent?.type ?? null }];
  while (stack.length > 0) {
    const { node, parentType } = stack.pop();
    if (node.isMissing || node.startIndex === node.endIndex) continue;
    if (node.type === "head_whitespace") continue;
    if (node.type === "ambiguous_lbracket_star" && parentType !== "type_param_group") {
      leaves.push({ node, kind: "operator.user" });
      continue;
    }
    if (TS_KIND_MAP[node.type] !== undefined) {
      const rawCallType =
        node.type === "identifier" &&
        (parentType === "call_expr" || parentType === "ufcs_expr") &&
        EXACT_TYPE_NAME.test(source.slice(node.startIndex, node.endIndex));
      leaves.push({
        node,
        kind: rawCallType ? "entity.name.type" : treeSitterKind(node),
      });
      continue;
    }
    if (node.childCount === 0) {
      if (
        node.isNamed &&
        node.type !== "source_file" &&
        TREE_SITTER_NODE_KIND.test(node.type)
      ) {
        throw new Error(
          `unmapped tree-sitter node kind: \`${node.type}\` ` +
            `(add to TS_KIND_MAP in check.mjs)`,
        );
      }
      continue;
    }
    const children = node.children;
    for (let i = children.length - 1; i >= 0; i--) {
      stack.push({ node: children[i], parentType: node.type });
    }
  }
  return leaves;
}

function treeSitterTokens(parser, source) {
  parser.reset();
  const tree = parser.parse(source);
  if (tree === null) {
    throw new Error("tree-sitter parser returned no tree");
  }
  try {
    return treeSitterLeaves(tree.rootNode, source).map(({ node, kind }) => ({
      start: node.startIndex,
      end: node.endIndex,
      kind,
      nodeType: node.type,
    }));
  } finally {
    tree.delete();
  }
}

function treeSitterKind(node) {
  const parentType = node.parent?.type;
  if (["ERROR", "unowned_function_head"].includes(parentType) && /^(keyword_|entity_name_|variable_parameter$)/.test(node.type)) {
    return "identifier";
  }
  if (node.type === "keyword_import") {
    return ["import_decl", "incomplete_import_decl"].includes(parentType) && node.parent.firstNamedChild?.id === node.id
      ? "keyword.declaration"
      : "identifier";
  }
  if (node.type === "keyword_op" || node.type === "keyword_varop") {
    return ["op_decl", "incomplete_op_decl", "variadic_decl", "incomplete_variadic_decl", "import_operator_item"].includes(parentType)
      ? "keyword.declaration"
      : "identifier";
  }
  if (node.type !== "keyword_rec") return TS_KIND_MAP[node.type];

  const parent = node.parent;
  const next = node.nextNamedSibling;
  if (parent === null) return "identifier";
  if ([
    "recursive_function_definition",
    "recursive_function_group",
    "recursive_call_expression",
    "incomplete_recursive_call_expression",
  ].includes(parent.type)) {
    return "keyword.declaration";
  }
  if (parent.type === "type_rec_group" && parent.firstNamedChild?.id === node.id) {
    return "keyword.declaration";
  }
  if (
    (parent.type === "newtype_decl_head" && next?.type === "keyword_newtype") ||
    (parent.type === "labels_decl_head" && next?.type === "keyword_labels")
  ) {
    return "keyword.declaration";
  }
  return "identifier";
}

// ---- TextMate side -------------------------------------------
//
// vscode-textmate is the same engine VS Code uses internally;
// loading kio.tmLanguage.json and tokenizing line-by-line gives
// us the exact scope assignments a VS Code user would see.

const onigWasm = fs.readFileSync(
  path.dirname(fileURLToPath(import.meta.resolve("vscode-oniguruma"))) +
    "/onig.wasm",
);

const onigLib = oniguruma.loadWASM(onigWasm).then(() => ({
  createOnigScanner: (sources) => new oniguruma.OnigScanner(sources),
  createOnigString: (s) => new oniguruma.OnigString(s),
}));

const registry = new vsctm.Registry({
  onigLib,
  loadGrammar: async (scopeName) => {
    if (scopeName === "source.kio") {
      const data = await fs.promises.readFile(tmGrammarPath, "utf8");
      return vsctm.parseRawGrammar(data, tmGrammarPath);
    }
    return null;
  },
});

// Map TextMate scope prefixes → canonical TokenKind strings. The
// first matching prefix wins. TextMate emits a stack of scopes
// per token; we look at the most-specific scope on top.
const TM_SCOPE_MAP = [
  ["comment.line.documentation", "comment.doc"],
  ["comment.line", "comment.line"],
  ["string.quoted", "literal.string"],
  ["constant.numeric", "literal.number"],
  ["constant.language.boolean", "literal.bool"],
  ["entity.name.function.macro.elaborator", "keyword.elaborator"],
  ["keyword.control", "keyword.control"],
  ["keyword.declaration.fn-marker", "keyword.declaration"],
  ["keyword.declaration", "keyword.declaration"],
  ["keyword.operator.arrow", "operator.builtin"],
  ["keyword.operator.user", "operator.user"],
  ["keyword.operator", "operator.builtin"],
  ["punctuation.bracket", "punctuation.bracket"],
  ["punctuation.separator", "punctuation.separator"],
  ["entity.name.label", "entity.name.label"],
  ["variable.parameter.slot", "slot"],
  ["variable.parameter", "variable.parameter"],
  ["variable.other", "identifier"],
];

function mapTmScopes(scopes) {
  // Walk inner→outer; first matching prefix wins.
  for (let i = scopes.length - 1; i >= 0; i--) {
    const scope = scopes[i];
    for (const [prefix, kind] of TM_SCOPE_MAP) {
      if (scope.startsWith(prefix + ".") || scope === prefix) return kind;
    }
  }
  return null;
}

async function loadTextMateGrammar() {
  const grammar = await registry.loadGrammar("source.kio");
  if (!grammar) throw new Error("failed to load kio.tmLanguage.json");
  return grammar;
}

function textmateTokens(grammar, source) {
  const lines = source.split("\n");
  const tokens = [];
  let state = vsctm.INITIAL;
  let lineStart = 0;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const result = grammar.tokenizeLine(line, state);
    for (const t of result.tokens) {
      const kind = mapTmScopes(t.scopes);
      if (kind === null) continue;
      tokens.push({
        start: lineStart + t.startIndex,
        end: lineStart + t.endIndex,
        kind,
      });
    }
    state = result.ruleStack;
    lineStart += line.length + 1; // +1 for the newline
  }
  return tokens;
}

function textmateScopesAt(grammar, source, offset) {
  const lines = source.split("\n");
  let state = vsctm.INITIAL;
  let lineStart = 0;
  for (const line of lines) {
    const result = grammar.tokenizeLine(line, state);
    if (lineStart <= offset && offset <= lineStart + line.length) {
      const local = offset - lineStart;
      return (
        result.tokens.find(
          (token) => token.startIndex <= local && local < token.endIndex,
        )?.scopes ?? []
      );
    }
    state = result.ruleStack;
    lineStart += line.length + 1;
  }
  return [];
}

function checkTextMateLabelSpelling(grammar) {
  const source = "import labels({itemName});";
  const labels = textmateTokens(grammar, source).filter(
    (token) => token.kind === "entity.name.label",
  );
  if (labels.length > 0) {
    throw new Error(
      "TextMate must not classify a non-lowercase label spelling as label syntax",
    );
  }
}

function checkTextMateLabelFixture(grammar, name, source, tokens) {
  if (name !== "30_import_label_item") return [];
  const checks = [
    ["selected_label", source.indexOf("selected_label"), "entity.name.label"],
    [
      "expression shorthand",
      source.lastIndexOf("shorthand_label"),
      "entity.name.label",
    ],
    ["ordinary block", source.lastIndexOf("ordinary_value"), "identifier"],
    [
      "contextual import label argument",
      source.lastIndexOf("{value}") + "{".length,
      "entity.name.label",
    ],
  ];
  const failures = [];
  for (const [description, offset, expected] of checks) {
    const token = tokens.find((token) => token.start <= offset && offset < token.end);
    if (token?.kind !== expected) {
      failures.push(
        `${description} at byte ${offset}: expected \`${expected}\`, got \`${token?.kind ?? "silence"}\``,
      );
    }
  }
  for (const [description, offset] of [
    ["contextual import declaration", source.indexOf("fn import") + "fn ".length],
    ["contextual import call", source.lastIndexOf("import(")],
  ]) {
    const scopes = textmateScopesAt(grammar, source, offset);
    const isFunction = scopes.some(
      (scope) =>
        scope === "entity.name.function.kio" ||
        scope.startsWith("entity.name.function.kio."),
    );
    const isDeclarationKeyword = scopes.some(
      (scope) =>
        scope === "keyword.declaration.kio" ||
        scope.startsWith("keyword.declaration.kio."),
    );
    if (!isFunction || isDeclarationKeyword) {
      failures.push(
        `${description} at byte ${offset}: expected function scope without declaration-keyword scope, got ${JSON.stringify(scopes)}`,
      );
    }
  }
  return failures;
}

// ---- Driver --------------------------------------------------

// TextMate's lexical comparison ignores entity refinements symmetrically;
// the raw-scope fixtures independently require their structural role scopes.
// Tree-sitter receives no normalization or span exemptions.
const TEXTMATE_NORMALIZATION = new Set([
  "entity.name.module",
  "entity.name.function",
  "entity.name.type",
  "entity.name.label",
  "variable.parameter",
]);

function normalizeFor(tokens, set) {
  return tokens.map((t) =>
    set.has(t.kind) ? { ...t, kind: "identifier" } : t,
  );
}

function treeSitterTokensAgree(reference, treeSitter) {
  return reference.length === treeSitter.length && reference.every((token, index) =>
    token.start === treeSitter[index].start && token.end === treeSitter[index].end &&
    token.kind === treeSitter[index].kind);
}

function fmtTokens(tokens) {
  return tokens.map((t) => `  {${t.start}-${t.end}: ${t.kind}}`).join("\n");
}

// Check that TextMate's positive classifications agree with the
// reference. TextMate may be silent on tokens it can't classify
// (regex limit, e.g. an op-token at an ambiguous position); only
// positive classifications must match.
//
// "Match" here means: for each TextMate-emitted token, there's a
// reference token spanning the same source bytes with the same
// kind. TextMate's token boundaries can fragment a reference
// token (e.g., per-char classification), so we walk byte-by-byte.
function checkTextMateAgreement(refTokens, tmTokens, source) {
  // Build a byte → kind table from the reference.
  const refKindAt = new Array(source.length).fill(null);
  for (const t of refTokens) {
    for (let b = t.start; b < t.end; b++) refKindAt[b] = t.kind;
  }
  const failures = [];
  for (const t of tmTokens) {
    for (let b = t.start; b < t.end; b++) {
      const refKind = refKindAt[b];
      if (refKind === null) continue; // outside any reference token
      if (refKind !== t.kind) {
        failures.push(
          `byte ${b}: textmate says \`${t.kind}\` but reference says \`${refKind}\``,
        );
      }
    }
  }
  return failures;
}

async function checkFixture(treeSitterParser, tmGrammar, fixtureDir) {
  const name = path.basename(fixtureDir);
  const sourcePath = path.join(fixtureDir, "source.kio");
  const source = fs.readFileSync(sourcePath, "utf8");

  // (1) Reference — re-derive via `kio debug tokens` so we
  // exercise the same dump path the per-PR CI uses. The checked-in
  // expected.tokens.json is the same dump, pinned elsewhere;
  // re-deriving through `kio` here keeps the agreement check
  // honest about end-to-end behavior.
  const { stdout: refRaw } = await execFileAsync(
    kioBin,
    ["debug", "tokens", sourcePath],
    {
      encoding: "utf8",
    },
  );
  const refTokens = JSON.parse(refRaw);

  const refForTextMate = normalizeFor(refTokens, TEXTMATE_NORMALIZATION);

  // (2) Tree-sitter — every kind and every token boundary must agree.
  const tsTokens = treeSitterTokens(treeSitterParser, source);
  if (!treeSitterTokensAgree(refTokens, tsTokens)) {
    return {
      name,
      kind: "tree-sitter",
      message:
        `tree-sitter disagrees with kio debug tokens:\n` +
        `reference:\n${fmtTokens(refTokens)}\n` +
        `tree-sitter:\n${fmtTokens(tsTokens)}`,
    };
  }

  // (3) TextMate — silence-allowed agreement at the lexical layer.
  // Bilateral normalization here too — TextMate today emits only
  // `identifier` for most refined positions and a structural label scope
  // for selective-import items. Applying the rewrite to both sides keeps that
  // partial refinement symmetric.
  const tmTokens = textmateTokens(tmGrammar, source);
  const tmFixtureFailures = checkTextMateLabelFixture(
    tmGrammar,
    name,
    source,
    tmTokens,
  );
  if (tmFixtureFailures.length > 0) {
    return {
      name,
      kind: "textmate",
      message:
        `textmate label fixture expectations failed:\n` +
        tmFixtureFailures.map((failure) => `  ${failure}`).join("\n"),
    };
  }
  const tmLabelFailures = checkTextMateAgreement(
    refTokens,
    tmTokens.filter((token) => token.kind === "entity.name.label"),
    source,
  );
  if (tmLabelFailures.length > 0) {
    return {
      name,
      kind: "textmate",
      message:
        `textmate labels disagree with parsed label positions at:\n` +
        tmLabelFailures.map((f) => `  ${f}`).join("\n"),
    };
  }
  const tmTokensNormalized = normalizeFor(tmTokens, TEXTMATE_NORMALIZATION);
  const tmFailures = checkTextMateAgreement(
    refForTextMate,
    tmTokensNormalized,
    source,
  );
  if (tmFailures.length > 0) {
    return {
      name,
      kind: "textmate",
      message:
        `textmate disagrees with kio debug tokens at:\n` +
        tmFailures.map((f) => `  ${f}`).join("\n"),
    };
  }

  return null;
}

function defaultConcurrencyLimit() {
  const available =
    typeof os.availableParallelism === "function"
      ? os.availableParallelism()
      : os.cpus().length;
  return Math.max(1, available || 1);
}

async function runWithConcurrency(items, limit, fn) {
  const results = new Array(items.length);
  let next = 0;
  const workerCount = Math.min(items.length, Math.max(1, limit));
  const workers = Array.from({ length: workerCount }, async () => {
    for (;;) {
      const index = next++;
      if (index >= items.length) return;
      results[index] = await fn(items[index], index);
    }
  });
  await Promise.all(workers);
  return results;
}

async function main() {
  const fixtures = fs
    .readdirSync(corpusDir, { withFileTypes: true })
    .filter((e) => e.isDirectory())
    .map((e) => path.join(corpusDir, e.name))
    .sort();

  const [treeSitterParser, tmGrammar] = await Promise.all([
    loadTreeSitterParser(),
    loadTextMateGrammar(),
  ]);
  const missingSourceDir = fs.mkdtempSync(path.join(os.tmpdir(), "kio-highlight-missing-source-"));
  try {
    await assert.rejects(
      () => checkFixture(treeSitterParser, tmGrammar, missingSourceDir),
      { code: "ENOENT" },
      "a fixture without source.kio must fail, not count as agreement",
    );
  } finally {
    fs.rmdirSync(missingSourceDir);
  }
  checkTextMateLabelSpelling(tmGrammar);
  checkTextMateHighlighting(tmGrammar);
  const repoRoot = new URL("../../../", import.meta.url);
  checkStructuralHighlighting(
    treeSitterParser,
    tmGrammar,
    [
      ...JSON.parse(fs.readFileSync(new URL("tools/vscode-kio/test/fixtures/structural-highlighting.json", repoRoot), "utf8")),
      ...JSON.parse(fs.readFileSync(new URL("tools/tree-sitter-kio/test/structural.json", repoRoot), "utf8")),
    ],
    fs.readFileSync(
      new URL("tools/tree-sitter-kio/queries/highlights.scm", repoRoot), "utf8",
    ),
    undefined,
    treeSitterLeaves,
  );

  checkIncrementalHighlighting(treeSitterParser, fs.readFileSync(
    new URL("tools/tree-sitter-kio/queries/highlights.scm", repoRoot), "utf8"));

  const results = await runWithConcurrency(
    fixtures,
    defaultConcurrencyLimit(),
    async (dir) => {
      try {
        return {
          failure: await checkFixture(treeSitterParser, tmGrammar, dir),
        };
      } catch (err) {
        return {
          failure: {
            name: path.basename(dir),
            kind: "exception",
            message: String(err),
            detail: err.stack ?? String(err),
          },
        };
      }
    },
  );

  let passes = 0;
  const failures = [];
  for (const { failure } of results) {
    if (failure) {
      failures.push(failure);
      console.error(`FAIL: ${failure.name} [${failure.kind}]`);
      if (failure.kind === "exception") {
        console.error(`  ${failure.detail}`);
      } else {
        console.error(failure.message.replace(/^/gm, "  "));
      }
    } else {
      passes++;
    }
  }

  console.log(
    `\nhighlight-agreement: ${passes} passed, ${failures.length} failed`,
  );
  treeSitterParser.delete();
  if (failures.length > 0) process.exit(1);
}

await main();
