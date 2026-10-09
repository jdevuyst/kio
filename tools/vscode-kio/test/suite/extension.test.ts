// End-to-end smoke tests for the Kio extension. Run inside a
// real VS Code instance via `@vscode/test-electron`.
//
// The assertions are layered the same way the extension is:
//
//   1. Activation — the extension loads without throwing on any
//      `.kio` file open.
//   2. TextMate floor — the bundled grammar registers and paints
//      on file open. We verify this via the language id and the
//      Markdown TextMate test (`getSemanticTokens` would return
//      tree-sitter output, not TextMate; we just check the
//      language id resolves).
//   3. Tree-sitter overlay — `vscode.executeDocumentSemanticTokensProvider`
//      returns a non-empty token set, proving the provider
//      registered and the WASM loaded.
//   4. LSP client — the extension spawns the configured
//      `kio.server.path` (a mock LSP server in this test) and
//      surfaces its `publishDiagnostics` notifications in VS
//      Code's diagnostic registry.
//
// `runTest.ts` writes `.vscode/settings.json` in the launch
// workspace with `kio.server.path` pointing at
// `fixtures/mock-lsp.js`, so when the extension activates on the
// first `.kio` open it spawns the mock and the publishDiagnostics
// path becomes observable.

import * as assert from "node:assert";
import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

// Initial server configuration for this launch — see `runTest.ts`.
const MODE = process.env.VSCODE_KIO_E2E_MODE ?? "mock";

suite("Kio extension — isolated native tooling", () => {
  test("built-in AI features are disabled", () => {
    assert.strictEqual(vscode.workspace.getConfiguration("chat").get("disableAIFeatures"), true);
    for (const id of ["GitHub.copilot", "GitHub.copilot-chat"]) {
      assert.notStrictEqual(vscode.extensions.getExtension(id)?.isActive, true, id);
    }
  });
});

function workspaceDir(): string {
  const dir = process.env.VSCODE_KIO_TEST_WORKSPACE;
  assert.ok(
    dir !== undefined && dir !== "",
    "VSCODE_KIO_TEST_WORKSPACE must be set by runTest.ts",
  );
  return dir;
}

async function openFixture(name: string): Promise<vscode.TextDocument> {
  const fixturePath = path.join(workspaceDir(), name);
  const uri = vscode.Uri.file(fixturePath);
  const doc = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(doc);
  return doc;
}

type DecodedSemanticToken = {
  text: string;
  type: number;
  line: number;
  character: number;
};

function decodedSemanticTokens(
  doc: vscode.TextDocument,
  tokens: vscode.SemanticTokens,
): DecodedSemanticToken[] {
  const decoded: DecodedSemanticToken[] = [];
  let line = 0;
  let character = 0;
  for (let offset = 0; offset < tokens.data.length; offset += 5) {
    const lineDelta = tokens.data[offset];
    const charDelta = tokens.data[offset + 1];
    const length = tokens.data[offset + 2];
    const type = tokens.data[offset + 3];
    if (lineDelta === 0) {
      character += charDelta;
    } else {
      line += lineDelta;
      character = charDelta;
    }
    decoded.push({
      text: doc.getText(
        new vscode.Range(line, character, line, character + length),
      ),
      type,
      line,
      character,
    });
  }
  return decoded;
}

function semanticTokenTypeAt(
  doc: vscode.TextDocument,
  decoded: DecodedSemanticToken[],
  needle: string,
  withinNeedle = 0,
): number | undefined {
  const needleOffset = doc.getText().indexOf(needle);
  assert.ok(needleOffset >= 0, `fixture contains ${needle}`);
  const expected = doc.positionAt(needleOffset + withinNeedle);
  return decoded.find(
    (token) =>
      token.line === expected.line && token.character === expected.character,
  )?.type;
}

/// Poll `predicate` every 100ms until it returns true or `timeoutMs`
/// elapses. Throws if the timeout fires.
async function waitFor(
  predicate: () => boolean,
  timeoutMs: number,
  description: string,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  assert.fail(`timed out after ${timeoutMs}ms waiting for: ${description}`);
}

async function completeEditorRequest<T>(
  request: () => Thenable<T>,
  version: () => number,
  deadline: number,
  pause: () => Promise<void> = () => new Promise((resolve) => setTimeout(resolve, 100)),
  now: () => number = Date.now,
): Promise<T> {
  const expectedVersion = version();
  for (;;) {
    try {
      const result = await request();
      assert.strictEqual(version(), expectedVersion, "the action request uses an unchanged document");
      return result;
    } catch (error) {
      // Editor invalidation can cancel a request while the document opens
      // or its applied edit propagates. Cancellation is not an empty result.
      const canceled = error instanceof vscode.CancellationError
        || (error instanceof Error && error.name === "Canceled" && error.message === "Canceled");
      if (!canceled || now() >= deadline) throw error;
      await pause();
    }
  }
}

// The activation / highlighting floor is server-independent, so it runs
// in the mock launch only rather than three times over.
if (MODE === "mock")
suite("Kio extension — activation & highlighting", () => {
  test("a .kio file resolves to the kio language id", async () => {
    const doc = await openFixture("minimal.kio");
    assert.strictEqual(doc.languageId, "kio");
  });

  test("Kio-family variant files resolve to the kio language id", async () => {
    for (const fixture of [
      "signature.sig.kio",
      "dependency.dep.kio",
      "dependency.lock.kio",
    ]) {
      const doc = await openFixture(fixture);
      assert.strictEqual(doc.languageId, "kio", fixture);
    }
  });

  test("semantic tokens provider classifies UFCS roles in both directions", async () => {
    const doc = await openFixture("highlight.kio");
    await vscode.extensions.getExtension("kio-lang.kio")!.activate();
    const tokens = (await vscode.commands.executeCommand(
      "vscode.provideDocumentSemanticTokens",
      doc.uri,
    )) as vscode.SemanticTokens | undefined;
    assert.ok(tokens, "expected SemanticTokens object");
    const decoded = decodedSemanticTokens(doc, tokens);
    const typesFor = (text: string): number[] =>
      decoded.filter((token) => token.text === text).map((token) => token.type);

    // Legend positions are fixed by `SEMANTIC_KINDS` in extension.ts:
    // macro=1, type=2, variable=3, operator=4, function=11.
    assert.strictEqual(semanticTokenTypeAt(doc, decoded, "Box.un_box"), 2);
    assert.strictEqual(semanticTokenTypeAt(doc, decoded, "_Box.un_marked"), 2);
    assert.strictEqual(
      semanticTokenTypeAt(doc, decoded, "un_box(A, Box", "un_box(".length),
      2,
    );
    assert.strictEqual(
      semanticTokenTypeAt(
        doc,
        decoded,
        "un_marked(_A, _Box",
        "un_marked(".length,
      ),
      2,
    );
    assert.ok(
      typesFor("receiver").filter((type) => type === 3).length >= 8,
      "inserted and ordinary receiver uses remain variables",
    );
    assert.ok(typesFor("pick").includes(11), "ordinary UFCS callee is a function");
    assert.deepStrictEqual(typesFor("member"), [11, 11]);
    assert.deepStrictEqual(typesFor("transform!"), [1, 1]);
    assert.deepStrictEqual(typesFor("_transform!"), [1]);
    for (const arrow of [".>", ".>>", ".<", ".<<"]) {
      assert.ok(
        typesFor(arrow).every((type) => type === 4),
        `${arrow} must be an operator`,
      );
      assert.ok(typesFor(arrow).length > 0, `${arrow} must be tokenized`);
    }
  });

  test("semantic tokens provider handles Kio-family variant files", async () => {
    for (const fixture of [
      "signature.sig.kio",
      "dependency.dep.kio",
      "dependency.lock.kio",
    ]) {
      const doc = await openFixture(fixture);
      await vscode.extensions.getExtension("kio-lang.kio")!.activate();
      const tokens = (await vscode.commands.executeCommand(
        "vscode.provideDocumentSemanticTokens",
        doc.uri,
      )) as vscode.SemanticTokens | undefined;
      assert.ok(tokens, `expected SemanticTokens object for ${fixture}`);
      assert.ok(tokens.data.length > 0, `expected non-empty token data for ${fixture}`);
    }
  });

  const structuralFixtures = [
    ...JSON.parse(fs.readFileSync(
      path.resolve(__dirname, "../../fixtures/structural-highlighting.json"), "utf8")),
    ...JSON.parse(fs.readFileSync(
      path.resolve(__dirname, "../../../../tree-sitter-kio/test/structural.json"), "utf8")),
    ...JSON.parse(fs.readFileSync(
      path.resolve(__dirname, "../../../../tree-sitter-kio/test/documentation.json"), "utf8")),
  ] as Array<{
    name: string;
    source: string;
    lineLocalNeutral?: string[];
    targets: Array<{ start: number; end: number; text: string; kind: string; vscodeKind?: string; vscodeTargets?: Array<{ start: number; end: number; kind: string }> }>;
  }>;
  const structuralKinds: Record<string, number> = {
    "keyword.control": 0,
    "keyword.declaration": 0,
    "keyword.elaborator": 1,
    "entity.name.type": 2,
    "identifier": 3,
    "variable.parameter": 9,
    "entity.name.module": 10,
    "entity.name.function": 11,
    "entity.name.label": 12,
    "operator.user": 4,
    "operator.builtin": 4,
    "punctuation.bracket": 8,
    "punctuation.separator": 8,
    "slot": 0,
    "comment.line": 7,
    "comment.doc": 7,
    "comment.doc.reference": 3,
    "comment.doc.directive": 0,
    "comment.doc.fence": 0,
    "comment.doc.code": 5,
    "comment.doc.heading": 0,
    "comment.doc.strong": 5,
    "comment.doc.emphasis": 5,
    "comment.doc.marker": 8,
    "comment.doc.link": 5,
    "comment.doc.attribute": 0,
    "literal.number": 6,
    "literal.bool": 6,
    "literal.string": 5,
  };
  const docStyling = JSON.parse(fs.readFileSync(
    path.resolve(__dirname, "../../../../tree-sitter-kio/test/documentation-styling.json"), "utf8")) as Array<{
      name: string; source: string; targets: Array<{ text: string; kind: string }>;
    }>;
  for (const fixture of docStyling) {
    test(`bundled documentation styling: ${fixture.name}`, async () => {
      const doc = await vscode.workspace.openTextDocument({ language: "kio", content: fixture.source });
      try {
        await vscode.window.showTextDocument(doc);
        const tokens = await vscode.commands.executeCommand<vscode.SemanticTokens>("vscode.provideDocumentSemanticTokens", doc.uri);
        assert.ok(tokens, fixture.name);
        const decoded = decodedSemanticTokens(doc, tokens).map((token) => {
          const start = doc.offsetAt(new vscode.Position(token.line, token.character));
          return { start, end: start + token.text.length, kind: token.type };
        });
        for (const target of fixture.targets) {
          const start = doc.getText().indexOf(target.text);
          assert.ok(start >= 0, `${fixture.name}: target exists`);
          for (let index = start; index < start + target.text.length; index++) {
            assert.ok(decoded.some((token) => token.start <= index && index < token.end && token.kind === structuralKinds[target.kind]),
              `${fixture.name}: ${target.text} at ${index} has ${target.kind}`);
          }
        }
      } finally { await vscode.commands.executeCommand("workbench.action.closeActiveEditor"); }
    });
  }
  for (const fixture of structuralFixtures) {
    test(`semantic tokens preserve structural roles: ${fixture.name}`, async () => {
      const failures: string[] = [];
      const doc = await vscode.workspace.openTextDocument({ language: "kio", content: fixture.source });
      try {
        await vscode.window.showTextDocument(doc);
        const documentSource = doc.getText();
        const eol = doc.eol === vscode.EndOfLine.CRLF ? "\r\n" : "\n";
        assert.strictEqual(documentSource, fixture.source.replace(/\r\n|\r|\n/g, eol),
          `${fixture.name}: editor document preserves source apart from its line endings`);
        // The editor normalizes mixed EOLs; compare exact spans in its document,
        // while the standalone parser fixtures retain their original byte spans.
        const documentOffset = (offset: number): number => {
          const lines = fixture.source.slice(0, offset).split(/\r\n|\r|\n/);
          return doc.offsetAt(new vscode.Position(lines.length - 1, lines.at(-1)!.length));
        };
        const tokens = (await vscode.commands.executeCommand(
          "vscode.provideDocumentSemanticTokens", doc.uri,
        )) as vscode.SemanticTokens | undefined;
        assert.ok(tokens, `${fixture.name}: expected SemanticTokens`);
        const decoded = decodedSemanticTokens(doc, tokens).map((token) => {
          const start = doc.offsetAt(new vscode.Position(token.line, token.character));
          return { start, end: start + token.text.length, type: token.type };
        });
        for (const target of fixture.targets) {
          const label = `${fixture.name}:${target.start} ${target.text}`;
          assert.strictEqual(fixture.source.slice(target.start, target.end), target.text, label);
          const start = documentOffset(target.start), end = documentOffset(target.end);
          assert.strictEqual(documentSource.slice(start, end), target.text, label);
          const expectedKind = target.vscodeKind ?? target.kind;
          assert.notStrictEqual(structuralKinds[expectedKind], undefined, expectedKind);
          const actual = decoded.filter((token) => token.start < end && start < token.end);
          if (target.vscodeTargets) {
            assert.deepStrictEqual(actual, target.vscodeTargets.map((part) => ({
              start: documentOffset(part.start), end: documentOffset(part.end), type: structuralKinds[part.kind],
            })), `${label}: full-context Kio refinement of the query's code-body floor`);
            continue;
          }
          if (fixture.lineLocalNeutral?.includes(target.text)) {
            assert.ok(actual.length === 1 && actual[0].type === structuralKinds["comment.doc"] && actual[0].start <= start && actual[0].end >= end,
              `${label}: line-local literal fallback stays neutral`);
            continue;
          }
          if (actual.length !== 1 || actual[0].start !== start ||
            actual[0].end !== end || actual[0].type !== structuralKinds[expectedKind]) {
            failures.push(`${label}: expected ${expectedKind}, received ${JSON.stringify(actual)}`);
          }
        }
        assert.deepStrictEqual(failures, [], failures.join("\n"));
      } finally {
        if (!doc.isClosed) {
          if (vscode.window.activeTextEditor?.document !== doc) {
            await vscode.window.showTextDocument(doc);
          }
          await vscode.commands.executeCommand("workbench.action.revertAndCloseActiveEditor");
          await waitFor(() => doc.isClosed, 5_000, `closing fixture ${fixture.name}`);
        }
      }
    });
  }

  test("semantic tokens retain roles across incremental delimiter and keyword edits", async () => {
    const source = "module source; fn run() { id([*F] F(.) -> F(.)) }";
    const doc = await vscode.workspace.openTextDocument({ language: "kio", content: source });
    await vscode.window.showTextDocument(doc);
    const read = async () => {
      const tokens = await vscode.commands.executeCommand<vscode.SemanticTokens>(
        "vscode.provideDocumentSemanticTokens", doc.uri);
      assert.ok(tokens);
      return decodedSemanticTokens(doc, tokens);
    };
    assert.ok((await read()).some((token) => token.text === "F" && token.type === 9));
    const replace = async (before: string, after: string) => {
      const start = doc.getText().indexOf(before);
      assert.ok(start >= 0, before);
      const edit = new vscode.WorkspaceEdit();
      edit.replace(doc.uri, new vscode.Range(doc.positionAt(start), doc.positionAt(start + before.length)), after);
      assert.strictEqual(await vscode.workspace.applyEdit(edit), true);
    };
    await replace("[*F] F(.) -> F(.)", "[* let(x) *]");
    assert.ok((await read()).some((token) => token.text === "let" && token.type === 11));
    await replace("let(x)", "if!(x) { x } else { x }");
    assert.ok((await read()).some((token) => token.text === "if!" && token.type === 1));
    assert.ok((await read()).some((token) => token.text === "else" && token.type === 0));
    await replace("if!(x) { x } else { x }", "if(x)");
    assert.ok((await read()).some((token) => token.text === "if" && token.type === 11));
    await replace("[* if(x) *]", "[*F] F(.) -> F(.)");
    assert.ok((await read()).some((token) => token.text === "F" && token.type === 9));
    await replace("[*F] F(.) -> F(.)", "rec");
    assert.ok((await read()).some((token) => token.text === "rec" && token.type === 3));
    await replace("rec", "rec(poly)");
    assert.ok((await read()).some((token) => token.text === "rec" && token.type === 11));
    assert.ok((await read()).some((token) => token.text === "poly" && token.type === 3));
    await replace("rec(poly)", "rec(poly) again(x)");
    assert.ok((await read()).some((token) => token.text === "rec" && token.type === 0));
    assert.ok((await read()).some((token) => token.text === "poly" && token.type === 0));
    await replace("rec(poly) again(x)", "rec(poly)");
    assert.ok((await read()).some((token) => token.text === "rec" && token.type === 11));
    assert.ok((await read()).some((token) => token.text === "poly" && token.type === 3));
    const balanced = "module probe; fn run() { .x. { x1 } pub(probe) fn neighbor() { () } }";
    await replace(doc.getText(), balanced);
    assert.ok((await read()).some((token) => token.text === "neighbor" && token.type === 3));
    await replace(balanced, balanced.slice(0, -2));
    assert.ok((await read()).some((token) => token.text === "neighbor" && token.type === 11));
    await replace(doc.getText(), balanced);
    assert.ok((await read()).some((token) => token.text === "neighbor" && token.type === 3));
  });
});

if (MODE === "mock")
suite("Kio extension — LSP client", () => {
  test("publishDiagnostics from the language server reach the editor", async () => {
    const doc = await openFixture("minimal.kio");
    // The mock server (see `fixtures/mock-lsp.js`) publishes one
    // diagnostic per didOpen notification with `source: "mock-lsp"`
    // and a fixed message. Wait for the language client to
    // initialize and forward the diagnostic.
    await waitFor(
      () => {
        const diags = vscode.languages.getDiagnostics(doc.uri);
        return diags.some((d) => d.source === "mock-lsp");
      },
      // 15s timeout: the language client's handshake takes a few
      // hundred ms in CI, but the first activation also has to
      // spin up the tree-sitter WASM concurrently. A generous
      // budget covers both without making a regression sit on a
      // 30s default Mocha timeout.
      15_000,
      "mock-lsp diagnostic to appear on minimal.kio",
    );
    const diags = vscode.languages
      .getDiagnostics(doc.uri)
      .filter((d) => d.source === "mock-lsp");
    assert.strictEqual(diags.length, 1);
    assert.strictEqual(
      diags[0]!.message,
      "mock diagnostic from the test harness",
    );
    assert.strictEqual(diags[0]!.severity, vscode.DiagnosticSeverity.Error);
  });
});

// The mock above proves the editor renders what a server publishes. It
// cannot prove the extension and the real server agree on how to start
// one — the mock answers whatever it is asked. This launch spawns the
// actual `kio lsp` against a real package and waits for its verdict.
if (MODE === "real")
suite("Kio extension — real kio lsp", () => {
  test("native quick fixes apply without an AI extension and leave capability choices explicit", async () => {
    for (const [name, source, expected, title] of [
      [
        "import",
        "module main; import support(\n// 😀 preserve this comment\nhepler); pub fn main() { helper() }\n",
        "module main; import support(\n// 😀 preserve this comment\nhelper); pub fn main() { helper() }\n",
        "Replace with `helper`",
      ],
      [
        "module-path",
        "module main; import supprt as chosen; pub fn main() { chosen.helper() }\n",
        "module main; import support as chosen; pub fn main() { chosen.helper() }\n",
        "Replace module path with `support`",
      ],
      [
        "call",
        "module main; import support as host; pub rec(host.drive) fn again(x: .) { again(x) }\n",
        "module main; import support as host; pub rec(host.drive) fn again(x: .) { rec again(x) }\n",
        "Mark recursive call with `rec`",
      ],
      [
        "qualified-head",
        "module main; import support as support; pub fn main() { suport. // preserve member\nhelper() }\n",
        "module main; import support as support; pub fn main() { support. // preserve member\nhelper() }\n",
        "Replace with `support`",
      ],
      [
        "label-import",
        "module main; import support({mrk}); pub fn main() { let _ = {mark = ()}; () }\n",
        "module main; import support({mark}); pub fn main() { let _ = {mark = ()}; () }\n",
        "Replace with `mark`",
      ],
      [
        "definition",
        "module main; pub fn again(x: .) { rec again(x) }\n",
        undefined,
        undefined,
      ],
    ] as const) {
      const relative = `native-fix-${name}`;
      const root = path.join(workspaceDir(), relative);
      fs.mkdirSync(root);
      fs.writeFileSync(path.join(root, "probe.pkg.kio"), "package probe;\n");
      fs.writeFileSync(path.join(root, "support.kio"),
        "module support; pub fn helper() { () } pub labels { mark: . }; host fn drive[S][R](step: S -> S | R, state: S) -> R;\n");
      fs.writeFileSync(path.join(root, "main.kio"), source);
      const doc = await openFixture(`${relative}/main.kio`);
      const errors = (): vscode.Diagnostic[] => vscode.languages.getDiagnostics(doc.uri)
        .filter((diagnostic) => diagnostic.source === "kio"
          && diagnostic.severity === vscode.DiagnosticSeverity.Error);
      await waitFor(() => errors().length > 0, 20_000, `${name} diagnostic`);
      assert.strictEqual(errors().length, 1);
      let requestSnapshot: string | undefined;
      const actions = await completeEditorRequest(() => {
        const requestedRange = errors()[0].range;
        requestSnapshot = JSON.stringify({
          uri: doc.uri.toString(),
          version: doc.version,
          range: requestedRange,
          diagnostics: errors(),
        });
        return vscode.commands.executeCommand<vscode.CodeAction[]>(
          "vscode.executeCodeActionProvider", doc.uri, requestedRange,
          vscode.CodeActionKind.QuickFix.value,
        );
      }, () => doc.version, Date.now() + 5_000);
      assert.ok(Array.isArray(actions), "the native provider completes the request");
      if (expected === undefined) {
        assert.match(errors()[0].message, /only allowed inside a `rec\(loop\)` function/);
        assert.deepStrictEqual(actions, [], "a function definition needs the author's loop capability");
        assert.strictEqual(doc.getText(), source);
        continue;
      }
      const action = actions.find((candidate) => candidate.title === title);
      assert.ok(action?.edit, action?.edit ? `${name}: a native edit is discoverable`
        : `${name}: a native edit is discoverable; ${JSON.stringify({
        expectedTitle: title,
        request: requestSnapshot,
        response: {
          version: doc.version,
          diagnostics: errors(),
          actions: actions.map((candidate) => ({
            title: candidate.title,
            kind: candidate.kind?.value,
            command: candidate.command,
            disabled: candidate.disabled,
            edit: candidate.edit?.entries().map(([uri, edits]) => ({
              uri: uri.toString(), edits,
            })),
          })),
        },
      })}`);
      assert.strictEqual(action.kind?.value, "quickfix");
      assert.strictEqual(action.command, undefined, "applying the fix needs no command or AI service");
      assert.strictEqual(await vscode.workspace.applyEdit(action.edit), true);
      assert.strictEqual(doc.getText(), expected);
      await waitFor(() => errors().length === 0, 20_000, `${name} repair reanalysis`);
    }
  });

  test("nested-call UFCS refactoring waits only for canceled editor requests", async () => {
    let tick = 0;
    const pause = async (): Promise<void> => { tick++; };
    const now = (): number => tick;
    let attempts = 0;
    const complete = await completeEditorRequest(async () => {
      if (attempts++ === 0) throw new vscode.CancellationError();
      return ["completed"];
    }, () => 1, 3, pause, now);
    assert.deepStrictEqual(complete, ["completed"]);
    assert.strictEqual(attempts, 2);
    const unrelated = new Error("provider failed");
    await assert.rejects(completeEditorRequest(async () => { throw unrelated; }, () => 1, 3, pause, now),
      (error) => error === unrelated);
    let version = 1;
    await assert.rejects(completeEditorRequest(async () => { version++; return []; }, () => version, 3, pause, now),
      /unchanged document/);
    tick = 0;
    attempts = 0;
    await assert.rejects(completeEditorRequest(async () => {
      attempts++;
      throw new vscode.CancellationError();
    }, () => 1, 3, pause, now), vscode.CancellationError);
    assert.strictEqual(attempts, 4);
    assert.strictEqual(tick, 3);
  });

  test("nested-call UFCS refactoring applies a whole imported host chain", async () => {
    const root = path.join(workspaceDir(), "ufcs-refactor");
    fs.mkdirSync(path.join(root, "demo"), { recursive: true });
    fs.writeFileSync(path.join(root, "demo.pkg.kio"), "package demo; bridge { demo/**; }\n");
    fs.writeFileSync(path.join(root, "demo/provider.kio"), "module demo/provider; host type Text role(str); host fn string_concat(a: Text, b: Text) -> Text;\n");
    const body = 'p.string_concat("a", string_concat("b", p.string_concat("c", "d")))';
    const source = `module demo/main; import demo/provider(Text, string_concat); import demo/provider as p; pub fn run() -> Text { ${body} }\n`;
    fs.writeFileSync(path.join(root, "demo/main.kio"), source);
    const doc = await openFixture("ufcs-refactor/demo/main.kio");
    const start = source.indexOf(body);
    const range = new vscode.Range(doc.positionAt(start), doc.positionAt(start + body.length));
    const query = (at: vscode.Range, deadline: number): Promise<vscode.CodeAction[] | undefined> =>
      completeEditorRequest(() => vscode.commands.executeCommand<vscode.CodeAction[]>(
        "vscode.executeCodeActionProvider", doc.uri, at, vscode.CodeActionKind.RefactorRewrite.value,
      ), () => doc.version, deadline);
    let action: vscode.CodeAction | undefined;
    const deadline = Date.now() + 20_000;
    while (!action && Date.now() < deadline) {
      const actions = await query(range, deadline);
      action = actions?.find((candidate) => candidate.title === "Convert nested calls to UFCS");
      if (!action) await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.ok(action?.edit, "the current clean document offers a versioned refactoring");
    assert.strictEqual(action.kind?.value, "refactor.rewrite");
    assert.strictEqual(await vscode.workspace.applyEdit(action.edit), true);
    assert.strictEqual(doc.getText(), source.replace(body, '("d".>>p.string_concat("c").>>string_concat("b").>>p.string_concat("a"))'));
    assert.strictEqual(vscode.languages.getDiagnostics(doc.uri).length, 0);
    const again = await query(
      new vscode.Range(doc.positionAt(start), doc.positionAt(doc.getText().lastIndexOf("}"))),
      Date.now() + 5_000,
    );
    assert.ok(Array.isArray(again), "the edited document receives a completed action response");
    assert.ok(!again.some((candidate) => candidate.title === "Convert nested calls to UFCS"));
  });

  test("a type error in the package reaches the editor as a diagnostic", async () => {
    const doc = await openFixture("lsp-pkg/main.kio");
    const fromKio = (): vscode.Diagnostic[] =>
      vscode.languages.getDiagnostics(doc.uri).filter((d) => d.source === "kio");

    // The server debounces analysis and then typechecks the package, so
    // this is slower than the mock's canned reply. A clean file publishes
    // nothing at all, so there is no empty publish to wait on — poll for
    // the diagnostic itself.
    await waitFor(
      () => fromKio().length > 0,
      20_000,
      "a `kio` diagnostic to appear on lsp-pkg/main.kio",
    );

    const diags = fromKio();
    assert.strictEqual(diags.length, 1, "expected exactly one diagnostic");
    assert.strictEqual(diags[0]!.severity, vscode.DiagnosticSeverity.Error);
    // `code` is kio's exit code for the error class; 14 is a type error.
    assert.strictEqual(diags[0]!.code, 14);
    assert.match(diags[0]!.message, /type mismatch/);
  });
});

// Initial probe rejection and absence of a spawn are observed at the
// terminal notification in availability.test.ts. This real editor launch
// verifies that highlighting survives selection of the stale binary.
if (MODE === "stale")
suite("Kio extension — a kio that cannot serve LSP", () => {
  test("the extension activates and highlights with a stale compiler", async () => {
    const doc = await openFixture("lsp-pkg/main.kio");

    const ext = vscode.extensions.getExtension("kio-lang.kio")!;
    await ext.activate();
    const tokens = await vscode.commands.executeCommand<vscode.SemanticTokens>(
      "vscode.provideDocumentSemanticTokens", doc.uri,
    );
    assert.ok(tokens && tokens.data.length > 0, "highlighting survives the stale compiler");

    assert.strictEqual(
      vscode.languages.getDiagnostics(doc.uri).length,
      0,
      "a server that cannot serve should publish nothing",
    );

    assert.ok(ext?.isActive, "the extension should still be active");
  });
});
