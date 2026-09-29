import * as assert from "node:assert";
import * as fs from "node:fs";
import { createRequire } from "node:module";
import * as path from "node:path";
import * as vm from "node:vm";
import * as vscode from "vscode";

if ((process.env.VSCODE_KIO_E2E_MODE ?? "mock") === "mock")
suite("Kio extension — extension host runtime", () => {
  for (const [name, uiKind, nodeHost] of [
    ["desktop UI with Node", vscode.UIKind.Desktop, true],
    ["browser UI with remote Node", vscode.UIKind.Web, true],
    ["browser UI without Node", vscode.UIKind.Web, false],
  ] as const) {
    test(name, async () => {
      const extension = vscode.extensions.getExtension("kio-lang.kio");
      assert.ok(extension);
      const entry = nodeHost ? extension.packageJSON.main : extension.packageJSON.browser;
      const bundlePath = path.join(extension.extensionPath, entry);
      const bundleRequire = createRequire(bundlePath);
      const subscriptions: vscode.Disposable[] = [];
      const lines: string[] = [];
      let provider: vscode.DocumentSemanticTokensProvider | undefined;
      const api = Object.create(vscode, {
        env: { value: Object.create(vscode.env, { uiKind: { value: uiKind } }) },
        window: { value: Object.create(vscode.window, {
          createOutputChannel: { value: (channelName: string) => ({
            name: channelName,
            append: (line: string) => lines.push(line),
            appendLine: (line: string) => lines.push(line),
            replace: (line: string) => lines.push(line),
            clear: () => {}, show: () => {}, hide: () => {}, dispose: () => {},
          }) },
        }) },
        languages: { value: Object.create(vscode.languages, {
          registerDocumentSemanticTokensProvider: { value: (
            selector: vscode.DocumentSelector,
            registered: vscode.DocumentSemanticTokensProvider,
            legend: vscode.SemanticTokensLegend,
          ) => {
            provider = registered;
            return vscode.languages.registerDocumentSemanticTokensProvider(selector, registered, legend);
          } },
        }) },
      });
      const module = { exports: {} as {
        activate(context: unknown): Promise<void>;
        deactivate(): Promise<void>;
      } };
      vm.runInNewContext(fs.readFileSync(bundlePath, "utf8"), {
        module,
        exports: module.exports,
        require: (id: string) => {
          if (id === "vscode") return api;
          assert.ok(nodeHost, `browser bundle requested Node module ${id}`);
          return bundleRequire(id);
        },
        ...(nodeHost ? { process, Buffer } : {}),
        console, URL, TextDecoder, TextEncoder, Uint8Array,
        setTimeout, clearTimeout, setInterval, clearInterval,
        setImmediate, clearImmediate, performance,
      }, {
        filename: bundlePath,
        importModuleDynamically: nodeHost ? vm.constants.USE_MAIN_CONTEXT_DEFAULT_LOADER : undefined,
      });

      try {
        await module.exports.activate({
          extensionUri: extension.extensionUri,
          subscriptions,
          workspaceState: { get: () => false, update: async () => {} },
        });
        assert.ok(provider, "bundled WASM registers the highlighting provider");
        const doc = await vscode.workspace.openTextDocument({
          language: "kio", content: "module example;\n",
        });
        const cancellation = new vscode.CancellationTokenSource();
        subscriptions.push(cancellation);
        const tokens = await provider.provideDocumentSemanticTokens(doc, cancellation.token);
        assert.ok(tokens && tokens.data.length > 0, "bundled grammar produces tokens");
        if (nodeHost) {
          const deadline = Date.now() + 10_000;
          while (!lines.includes("lsp: language client started") && Date.now() < deadline) {
            if (lines.some((line) => /lsp: (skipped|.*failed)/.test(line))) break;
            await new Promise((resolve) => setTimeout(resolve, 20));
          }
          assert.ok(lines.includes("lsp: language client started"), lines.join("\n"));
        } else {
          assert.ok(lines.some((line) => line.startsWith("lsp: skipped")), lines.join("\n"));
          assert.ok(!lines.some((line) => line.startsWith("lsp: starting")));
        }
      } finally {
        await module.exports.deactivate();
        for (const subscription of subscriptions.reverse()) subscription.dispose();
      }
    });
  }

  test("prefers the workspace extension host", () => {
    const extension = vscode.extensions.getExtension("kio-lang.kio");
    assert.strictEqual(extension?.packageJSON.extensionKind?.[0], "workspace");
  });
});
