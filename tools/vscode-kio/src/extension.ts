// VS Code extension for Kio.
//
// Three feature layers, layered for redundancy and progressive
// enhancement:
//
//   1. TextMate grammar — the load-fast floor. Wired through the
//      manifest's `contributes.grammars`, painted before this
//      file's activation code even runs.
//   2. Tree-sitter semantic-tokens overlay — refines the
//      TextMate baseline with parser-context distinctions (`fn` in
//      declaration vs. lambda position, etc.) once
//      `web-tree-sitter` loads the WASM. Each token tree-sitter
//      classifies overrides the TextMate scope at the same range.
//   3. Language Server Protocol client (Node extension hosts) — spawns
//      `kio lsp` as a child process and routes diagnostics and
//      LSP capabilities through `vscode-languageclient`. Layers
//      *additively*: when the server publishes diagnostics, VS Code
//      renders squiggles on top of the existing highlighting; the
//      TextMate floor and tree-sitter overlay keep painting
//      regardless of whether the server is found.
//
// Cross-tokenizer agreement with `kio debug tokens` (the
// reference) is verified at build time by
// `ci/checks/orchestrators/highlight-agreement.sh`; this extension just bundles
// the grammars and wires them into VS Code's API. The highlighting
// path runs identically on desktop and web (vscode.dev /
// github.dev) — no `node:*` imports, WASM loads via the
// extension's bundled assets. The LSP client (local or remote Node)
// lives in a separate module loaded lazily via `await import(...)`
// so browser-only hosts never evaluate its Node dependencies.

import * as vscode from "vscode";
import { Parser, Language, Node, Tree, Edit } from "web-tree-sitter";

// Token-kind enum used by the semantic-tokens legend. The order
// here is the wire-form integer the provider returns; entries
// must align with `LEGEND.tokenTypes` below.
const SEMANTIC_KINDS = [
  "keyword", // keyword.control + keyword.declaration
  "macro", // elaborator names
  "type", // type names and references
  "variable", // identifier
  "operator", // operator.builtin + operator.user
  "string", // literal.string
  "number", // literal.number + literal.bool (closest match)
  "comment", // comment.line
  "punctuation", // bracket / separator — folded into one
  "parameter", // variable.parameter — fn binders
  "namespace", // entity.name.module — module-path segments
  "function", // entity.name.function — fn / equiv definition names
  "property", // entity.name.label — label entry names inside `labels { … }`
] as const;
type SemanticKind = (typeof SEMANTIC_KINDS)[number];

const TYPE_NAME = /^_*[A-Z][a-z]*[0-9]*(?:_[a-z]+[0-9]*)*_*$/;
const VALUE_NAME = /^_?[a-z]+[0-9]*(?:_[a-z]+[0-9]*)*_*$/;

function isExactTypeName(name: string): boolean {
  return TYPE_NAME.test(name);
}

function isExactValueName(name: string): boolean {
  return VALUE_NAME.test(name);
}

const LEGEND = new vscode.SemanticTokensLegend(
  SEMANTIC_KINDS as readonly string[] as string[],
  [],
);

// Map tree-sitter grammar node names to their semantic-token
// kind. Mirrors the canonical token-kind vocabulary — the
// `TokenKind` enum in `kio-rs/src/tokens.rs`, the reference behind
// `kio debug tokens` (the source of truth). The agreement check
// verifies the kio-rs and grammar sides agree on the same
// vocabulary; here we collapse to VS Code's coarser semantic-token
// kinds.
const NODE_KIND: Record<string, SemanticKind> = {
  keyword_control: "keyword",
  keyword_declaration: "keyword",
  keyword_package: "keyword",
  keyword_module: "keyword",
  keyword_build: "keyword",
  keyword_host: "keyword",
  keyword_bridge: "keyword",
  keyword_import: "variable",
  keyword_op: "variable",
  keyword_varop: "variable",
  keyword_let: "keyword",
  keyword_as: "variable",
  keyword_type: "keyword",
  keyword_literal: "keyword",
  keyword_pub: "keyword",
  keyword_pure: "keyword",
  keyword_fn: "keyword",
  keyword_with: "keyword",
  keyword_signature: "keyword",
  keyword_dependency: "keyword",
  keyword_source: "keyword",
  keyword_lock: "keyword",
  keyword_resolved: "keyword",
  keyword_breaking: "keyword",
  keyword_nonbreaking: "keyword",
  keyword_add: "keyword",
  keyword_modify: "keyword",
  keyword_remove: "keyword",
  keyword_newtype: "keyword",
  keyword_labels: "keyword",
  keyword_equiv: "keyword",
  keyword_constructor: "keyword",
  keyword_projector: "keyword",
  elaborator_name: "macro",
  block_head: "macro",
  block_label: "keyword",
  // Module-path segments inside `module a/b;` and import provider paths
  // — surfaced by the structural `module_decl` / `import_decl`
  // productions. Maps to `namespace`, which most themes color
  // distinctly from plain identifiers.
  entity_name_module: "namespace",
  // Type definition names — surfaced by `newtype_decl_head` and
  // the named form of `labels_decl_head`. Maps to `type`, which is
  // the standard VS Code semantic-token type for type names.
  entity_name_type: "type",
  // Function definition names and structurally recognized direct/UFCS
  // callees. Maps to `function`, the standard VS Code semantic-token
  // type for function names.
  entity_name_function: "function",
  // Label entry names inside `labels { foo: T, bar: U };` — surfaced
  // by `label_entry`. Maps to `property`, VS Code's standard
  // semantic-token kind for struct-field-shaped names.
  entity_name_label: "property",
  // Parameter binders inside signatures (`fn foo[A](x: T)`) —
  // surfaced by `type_param_binder` and `value_param_binder`.
  // Parameter *uses* inside the body stay at `variable` until
  // expression grammar grows.
  variable_parameter: "parameter",
  identifier: "variable",
  uppercase_identifier: "variable",
  lowercase_identifier: "variable",
  operator_builtin: "operator",
  operator_arrow_type: "operator",
  operator_arrow_ufcs: "operator",
  operator_run: "operator",
  unowned_symbol_run: "operator",
  variadic_open: "operator",
  variadic_close: "operator",
  variadic_star_close: "operator",
  star_open_prefix: "punctuation",
  kind_annotation: "operator",
  // The `/` module-path separator — the canonical classifier paints
  // it `operator.user` (it is also the division operator), so the
  // overlay themes it as an operator.
  module_path_sep: "operator",
  string_literal: "string",
  number_literal: "number",
  bool_literal: "number",
  comment_line: "comment",
  comment_doc: "comment",
  bracket_lparen: "punctuation",
  bracket_rparen: "punctuation",
  bracket_lbrace: "punctuation",
  bracket_rbrace: "punctuation",
  bracket_lbracket: "punctuation",
  bracket_rbracket: "punctuation",
  separator_semicolon: "punctuation",
  separator_comma: "punctuation",
  separator_dot: "punctuation",
  // `_`, `__`, `___` — mapped to `keyword` so they
  // pick up the keyword color distinctly from `variable` and
  // `parameter`. Semantically these are syntactic placeholders,
  // not actual keywords, but among the standard VS Code
  // semantic-token types `keyword` is the most reliably distinct
  // across themes and reads as "this character is special syntax,
  // not a name".
  slot: "keyword",
};

class KioSemanticTokensProvider
  implements vscode.DocumentSemanticTokensProvider
{
  private parser: Parser;
  private trees = new Map<string, Tree>();
  private subscriptions: vscode.Disposable[];

  constructor(parser: Parser) {
    this.parser = parser;
    this.subscriptions = [
      vscode.workspace.onDidCloseTextDocument((document) => {
        const key = document.uri.toString();
        this.trees.get(key)?.delete();
        this.trees.delete(key);
      }),
      vscode.workspace.onDidChangeTextDocument((event) => {
        const tree = this.trees.get(event.document.uri.toString());
        if (tree === undefined) return;
        for (const change of [...event.contentChanges].sort((a, b) => b.rangeOffset - a.rangeOffset)) {
          const lines = change.text.split("\n");
          tree.edit(new Edit({
            startIndex: change.rangeOffset,
            oldEndIndex: change.rangeOffset + change.rangeLength,
            newEndIndex: change.rangeOffset + change.text.length,
            startPosition: { row: change.range.start.line, column: change.range.start.character },
            oldEndPosition: { row: change.range.end.line, column: change.range.end.character },
            newEndPosition: {
              row: change.range.start.line + lines.length - 1,
              column: lines.length === 1
                ? change.range.start.character + change.text.length
                : lines.at(-1)!.length,
            },
          }));
        }
      }),
    ];
  }

  dispose(): void {
    for (const subscription of this.subscriptions) subscription.dispose();
    for (const tree of this.trees.values()) tree.delete();
    this.trees.clear();
    this.parser.delete();
  }

  async provideDocumentSemanticTokens(
    document: vscode.TextDocument,
    _token: vscode.CancellationToken,
  ): Promise<vscode.SemanticTokens> {
    const builder = new vscode.SemanticTokensBuilder(LEGEND);
    const key = document.uri.toString();
    const old = this.trees.get(key);
    const tree = this.parser.parse(document.getText(), old);
    if (tree === null) return builder.build();
    old?.delete();
    this.trees.set(key, tree);
    walkTree(tree.rootNode, builder);
    return builder.build();
  }
}

function walkTree(node: Node, builder: vscode.SemanticTokensBuilder): void {
  if (["ERROR", "unowned_function_head"].includes(node.parent?.type ?? "") && /^(keyword_|entity_name_|variable_parameter$)/.test(node.type)) {
    emitToken(node, "variable", builder);
    return;
  }
  if (node.type === "ambiguous_lbracket_star" && node.parent?.type !== "type_param_group") {
    emitToken(node, "operator", builder);
    return;
  }
  if (node.type === "keyword_rec") {
    const parent = node.parent;
    const next = node.nextNamedSibling;
    const declaration = [
      "recursive_function_definition",
      "recursive_function_group",
      "recursive_call_expression",
      "incomplete_recursive_call_expression",
    ].includes(parent?.type ?? "") ||
      (parent?.type === "type_rec_group" && parent.firstNamedChild?.id === node.id) ||
      (parent?.type === "newtype_decl_head" && next?.type === "keyword_newtype") ||
      (parent?.type === "labels_decl_head" && next?.type === "keyword_labels");
    emitToken(node, declaration ? "keyword" : "variable", builder);
    return;
  }
  // Special case: a `call_callee` node's prefix identifiers are
  // the qualifying path leading to the function name. Per Kio's
  // spelling convention, exact type names (`Box`, `_Box`) identify type
  // qualifiers (`Box.mk_box`, `_Box.mk_box`) and value-shaped names identify
  // namespace qualifiers (`a.b.c.foo`). The tree-sitter grammar leaves the
  // prefix as plain `identifier` so the parser's literal-keyword
  // preference stays intact at top-level positions; this overlay
  // applies the casing rule directly. The trailing function name
  // is already aliased to `entity_name_function` by the grammar
  // and handled by the default leaf path below.
  if (node.type === "call_callee") {
    for (let i = 0; i < node.childCount; i++) {
      const child = node.child(i);
      if (child === null) continue;
      if (child.type === "identifier") {
        if (isExactTypeName(child.text)) {
          emitToken(child, "type", builder);
        } else if (isExactValueName(child.text)) {
          emitToken(child, "namespace", builder);
        } else {
          walkTree(child, builder);
        }
      } else {
        walkTree(child, builder);
      }
    }
    return;
  }

  // A node whose grammar-name maps to a semantic kind is emitted
  // as a single token covering its full extent. We deliberately
  // do NOT recurse afterwards: when a `keyword_declaration`-shaped
  // `op`/`let` is aliased to `entity_name_module` inside
  // `module_path`, the alias node has an anonymous child (the
  // literal `op`/`let` token), so `childCount > 0`. A
  // leaf-only emit path would skip the alias node and recurse
  // into the unmapped anonymous child, silently dropping the
  // token. Emitting on type-match handles that case cleanly.
  let kind = NODE_KIND[node.type];
  const parentType = node.parent?.type;
  if (node.type === "keyword_import") {
    kind = parentType !== undefined && ["import_decl", "incomplete_import_decl"].includes(parentType) && node.parent?.firstNamedChild?.id === node.id
      ? "keyword"
      : "variable";
  } else if (node.type === "keyword_op" || node.type === "keyword_varop") {
    kind = parentType !== undefined && ["op_decl", "incomplete_op_decl", "variadic_decl", "incomplete_variadic_decl", "import_operator_item"].includes(parentType)
      ? "keyword"
      : "variable";
  }
  if (kind !== undefined) {
    emitToken(node, kind, builder);
    return;
  }
  for (let i = 0; i < node.childCount; i++) {
    const child = node.child(i);
    if (child !== null) walkTree(child, builder);
  }
}

function emitToken(
  node: Node,
  kind: SemanticKind,
  builder: vscode.SemanticTokensBuilder,
): void {
  const typeIndex = SEMANTIC_KINDS.indexOf(kind);
  const start = node.startPosition;
  const end = node.endPosition;
  // VS Code's semantic-tokens API only supports single-line
  // tokens. Multi-line tokens (a block comment that spans
  // lines, a string with newlines) are left to the TextMate
  // floor.
  if (!node.isMissing && start.row === end.row && start.column < end.column) {
    builder.push(
      start.row,
      start.column,
      end.column - start.column,
      typeIndex,
      0,
    );
  }
}

// Tracked at module scope so `deactivate` can await the language
// client's clean-shutdown response (LSP `shutdown` then `exit`).
// VS Code's `Disposable.dispose` is synchronous, so the
// `context.subscriptions` path alone wouldn't await the response
// — which would leak the `kio lsp` child for the brief window
// between the dispose call and the OS reaping the orphan.
//
// The shape is `{ dispose, stop }` (see `lsp.ts`). We type it
// structurally here so `extension.ts` doesn't need to statically
// import the LSP module — the module is only resolved at runtime,
// in a Node extension host, via `await import("./lsp")` below.
let lspClientHandle:
  | { dispose: () => void; stop: () => Promise<void> }
  | undefined;

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  const output = vscode.window.createOutputChannel("Kio");
  context.subscriptions.push(output);
  output.appendLine(`kio extension activating (uri=${context.extensionUri.toString()})`);

  try {
    await activateInner(context, output);
    output.appendLine("kio extension activated cleanly");
  } catch (err) {
    output.appendLine(`kio extension activation failed:\n${formatError(err)}`);
    // Re-throw so VS Code still reports activation failure in the
    // status bar / command-pallete diagnostics; but the full stack
    // is now in the Output panel ("Kio" channel) where users will
    // actually see it.
    throw err;
  }

  // Codespaces can display a web UI while running this extension in
  // remote Node. Child-process support follows the runtime, not uiKind.
  // Start after highlighting without delaying extension activation.
  if (typeof process !== "undefined" && process.versions?.node !== undefined) {
    void startLspClient(context, output);
  } else {
    output.appendLine(
      "lsp: skipped (browser extension host has no child-process support)",
    );
  }
}

async function startLspClient(
  context: vscode.ExtensionContext,
  output: vscode.OutputChannel,
): Promise<void> {
  try {
    // esbuild includes the LSP module in the shared bundle, but its
    // Node dependencies are evaluated only when this import runs.
    const lsp = await import("./lsp");
    const handle = await lsp.startLanguageClient(context, output);
    if (handle !== undefined) {
      lspClientHandle = handle;
      context.subscriptions.push(handle);
    }
  } catch (err) {
    output.appendLine(`lsp: client wiring failed:\n${formatError(err)}`);
    // Don't re-throw — the highlighting layer has already activated
    // successfully, and a failure here should not surface as an
    // extension activation failure in the editor UI.
  }
}

async function activateInner(
  context: vscode.ExtensionContext,
  output: vscode.OutputChannel,
): Promise<void> {
  const kioWasmUri = vscode.Uri.joinPath(
    context.extensionUri,
    "parsers",
    "tree-sitter-kio.wasm",
  );
  const runtimeWasmUri = vscode.Uri.joinPath(
    context.extensionUri,
    "dist",
    "web-tree-sitter.wasm",
  );

  output.appendLine(`reading runtime wasm: ${runtimeWasmUri.toString()}`);
  const runtimeWasm = await vscode.workspace.fs.readFile(runtimeWasmUri);
  output.appendLine(`  ok (${runtimeWasm.byteLength} bytes)`);

  output.appendLine("compiling runtime wasm module");
  const runtimeModule = await WebAssembly.compile(Uint8Array.from(runtimeWasm));
  output.appendLine("  ok");

  output.appendLine("initializing tree-sitter parser");
  // Bypass Emscripten's fetch / fs / locateFile machinery entirely
  // by providing a pre-compiled `WebAssembly.Module` via the
  // `instantiateWasm` hook. The default loader resolves
  // `web-tree-sitter.wasm` against `import.meta.url`, which
  // esbuild replaces with an empty object — so the URL becomes
  // undefined and the fallback Node `fs.readFile(undefined)`
  // throws. Pre-compiling sidesteps the entire URL/fetch path.
  await Parser.init({
    instantiateWasm: (
      imports: WebAssembly.Imports,
      receive: (
        instance: WebAssembly.Instance,
        module: WebAssembly.Module,
      ) => void,
    ) => {
      WebAssembly.instantiate(runtimeModule, imports)
        .then((instance) => receive(instance, runtimeModule))
        .catch((err) => {
          output.appendLine(`wasm instantiate failed:\n${formatError(err)}`);
        });
      return {};
    },
  } as Parameters<typeof Parser.init>[0]);
  output.appendLine("  ok");

  output.appendLine(`reading kio grammar wasm: ${kioWasmUri.toString()}`);
  const kioWasm = await vscode.workspace.fs.readFile(kioWasmUri);
  output.appendLine(`  ok (${kioWasm.byteLength} bytes)`);

  output.appendLine("loading kio language");
  const language = await Language.load(kioWasm);
  output.appendLine("  ok");

  const parser = new Parser();
  parser.setLanguage(language);

  const provider = new KioSemanticTokensProvider(parser);
  context.subscriptions.push(
    provider,
    vscode.languages.registerDocumentSemanticTokensProvider(
      { language: "kio" },
      provider,
      LEGEND,
    ),
  );
  output.appendLine("semantic-tokens provider registered for `kio` language");
}

function formatError(err: unknown): string {
  if (err instanceof Error) {
    return `${err.name}: ${err.message}\n${err.stack ?? "(no stack)"}`;
  }
  return String(err);
}

export async function deactivate(): Promise<void> {
  // Stop the language client explicitly here (rather than relying
  // solely on `context.subscriptions` cleanup): `Disposable.dispose`
  // is synchronous, but `LanguageClient.stop()` returns a promise
  // that resolves only after the LSP `shutdown` / `exit` handshake
  // completes. Awaiting it from `deactivate` (which VS Code itself
  // awaits) ensures the spawned `kio lsp` child is reaped before
  // the extension host moves on — no orphaned processes on reload.
  if (lspClientHandle !== undefined) {
    const handle = lspClientHandle;
    lspClientHandle = undefined;
    await handle.stop();
  }
}
