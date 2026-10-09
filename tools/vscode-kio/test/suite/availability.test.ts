import * as assert from "node:assert";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import * as vm from "node:vm";
import { createRequire } from "node:module";
import * as childProcess from "node:child_process";
import * as vscode from "vscode";

const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check: () => boolean, description: string, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check() && Date.now() < deadline) await delay(20);
  assert.ok(check(), description);
}

async function host(serverPath: string | null, control: string, options: {
  environment?: Record<string, string | undefined>;
  choice?: string;
  state?: Map<string, unknown>;
  holdStartupDeadlines?: boolean;
} = {}) {
  const extension = vscode.extensions.getExtension("kio-lang.kio")!;
  const bundle = path.join(extension.extensionPath, extension.packageJSON.main);
  const bundleRequire = createRequire(bundle);
  const subscriptions: vscode.Disposable[] = [];
  const commands = new Map<string, (...args: unknown[]) => unknown>();
  const logs: string[] = [];
  const notifications: { severity: string; message: string; actions: string[] }[] = [];
  const statuses: vscode.LanguageStatusItem[] = [];
  const diagnostics = new Map<string, readonly vscode.Diagnostic[]>();
  const unhandled: unknown[] = [];
  const onUnhandled = (error: unknown) => unhandled.push(error);
  const configuration = new vscode.EventEmitter<vscode.ConfigurationChangeEvent>();
  subscriptions.push(configuration);
  let configured = serverPath;
  const heldDeadlines = new Set<ReturnType<typeof setTimeout>>();
  const probeSignals: AbortSignal[] = [];
  const startupChildren: childProcess.ChildProcess[] = [];
  const childProcessApi = Object.create(childProcess, {
    execFile: { value: (binary: string, args: string[], settings: childProcess.ExecFileOptions, callback: Parameters<typeof childProcess.execFile>[3]) => {
      assert.deepStrictEqual(Array.from(args), ["--help"], "only the startup probe is controlled");
      assert.strictEqual(settings.timeout, 5_000, "the probe deadline is held");
      assert.ok(settings.signal, "the real probe cancellation signal is retained");
      probeSignals.push(settings.signal);
      const child = childProcess.execFile(binary, args, { ...settings, timeout: 0 }, callback);
      startupChildren.push(child);
      return child;
    } },
    spawn: { value: (binary: string, args: string[], settings: childProcess.SpawnOptions) => {
      const child = childProcess.spawn(binary, args, settings);
      startupChildren.push(child);
      return child;
    } },
  });
  const startupSetTimeout: typeof setTimeout = ((callback: (...args: unknown[]) => void, ms?: number, ...args: unknown[]) => {
    if (ms !== 10_000) return setTimeout(callback, ms, ...args);
    // Initialization stays pending until lifecycle cancellation kills its child.
    const timer = setTimeout(() => {}, 0);
    clearTimeout(timer);
    heldDeadlines.add(timer);
    return timer;
  }) as typeof setTimeout;
  const api = Object.create(vscode, {
    commands: { value: Object.create(vscode.commands, {
      registerCommand: { value: (name: string, callback: (...args: unknown[]) => unknown) => {
        commands.set(name, callback);
        return { dispose: () => commands.delete(name) };
      } },
    }) },
    workspace: { value: Object.create(vscode.workspace, {
      getConfiguration: { value: (section?: string) => {
        const original = vscode.workspace.getConfiguration(section);
        return section !== "kio" ? original : Object.create(original, {
          get: { value: (key: string, fallback?: unknown) => key === "server.path" ? configured : key === "trace.server" ? "off" : original.get(key, fallback) },
        });
      } },
      onDidChangeConfiguration: { value: configuration.event },
    }) },
    window: { value: Object.create(vscode.window, {
      createOutputChannel: { value: (name: string) => ({ name,
        append: (text: string) => logs.push(text), appendLine: (text: string) => logs.push(text),
        clear() {}, show() {}, hide() {}, dispose() {},
      }) },
      ...Object.fromEntries(["Information", "Warning", "Error"].map((severity) => [
        `show${severity}Message`, { value: async (message: string, ...actions: string[]) => {
          notifications.push({ severity, message, actions });
          return options.choice;
        } },
      ])),
    }) },
    languages: { value: Object.create(vscode.languages, {
      createDiagnosticCollection: { value: (name?: string) => {
        const collection = vscode.languages.createDiagnosticCollection(name);
        return Object.create(collection, { set: { value: (uri: vscode.Uri, values: readonly vscode.Diagnostic[]) => {
          diagnostics.set(uri.toString(), values);
          collection.set(uri, values);
        } } });
      } },
      createLanguageStatusItem: { value: () => {
        const status = { dispose() {} } as vscode.LanguageStatusItem;
        statuses.push(status);
        return status;
      } },
    }) },
  });
  const module = { exports: {} as { activate(context: unknown): Promise<void>; deactivate(): Promise<void> } };
  vm.runInNewContext(fs.readFileSync(bundle, "utf8"), {
    module, exports: module.exports,
    require: (id: string) => id === "vscode" ? api
      : options.holdStartupDeadlines && id === "node:child_process" ? childProcessApi : bundleRequire(id),
    process: Object.create(process, { env: { value: { ...process.env, ...options.environment, KIO_TEST_LSP_CONTROL: control } } }),
    Buffer, console, URL, TextDecoder, TextEncoder, Uint8Array, AbortController,
    setTimeout: options.holdStartupDeadlines ? startupSetTimeout : setTimeout,
    clearTimeout: (timer: ReturnType<typeof setTimeout>) => { heldDeadlines.delete(timer); clearTimeout(timer); },
    setInterval, clearInterval, setImmediate, clearImmediate, performance,
  }, { filename: bundle, importModuleDynamically: vm.constants.USE_MAIN_CONTEXT_DEFAULT_LOADER });
  process.on("unhandledRejection", onUnhandled);
  const state = options.state ?? new Map<string, unknown>();
  let closing: Promise<void> | undefined;
  try {
    await module.exports.activate({ extensionUri: extension.extensionUri, extension,
      subscriptions, workspaceState: { get: (key: string) => state.get(key), update: async (key: string, value: unknown) => { state.set(key, value); } },
    });
  } catch (error) {
    try { await module.exports.deactivate(); }
    finally {
      for (const subscription of subscriptions.reverse()) subscription.dispose();
      process.off("unhandledRejection", onUnhandled);
    }
    throw error;
  }
  return { logs, notifications, statuses, commands, state, diagnostics, heldDeadlines, probeSignals, startupChildren,
    async configure(value: string) {
      configured = value;
      configuration.fire({ affectsConfiguration: (name: string) => name === "kio.server.path" });
    },
    close() {
      return closing ??= (async () => {
        try { await module.exports.deactivate(); }
        finally {
          for (const subscription of subscriptions.reverse()) subscription.dispose();
          // VS Code reports extension-host promise failures on a one-second timer.
          await delay(1100);
          process.off("unhandledRejection", onUnhandled);
        }
        assert.deepStrictEqual(unhandled, [], "lifecycle failures must not escape as unhandled rejections");
      })();
    },
  };
}

if ((process.env.VSCODE_KIO_E2E_MODE ?? "mock") === "mock")
suite("Kio extension — availability and recovery", () => {
  let directory: string;
  let control: string;
  let fixture: string;
  setup(() => {
    directory = fs.mkdtempSync(path.join(os.tmpdir(), "kio-lsp-availability-"));
    control = path.join(directory, "control.json");
    fixture = path.join(vscode.extensions.getExtension("kio-lang.kio")!.extensionPath, "test/fixtures/fault-lsp.js");
  });
  teardown(() => fs.rmSync(directory, { recursive: true, force: true }));

  const runningStatus = (running: Awaited<ReturnType<typeof host>>) => running.statuses[0]?.text === "Kio: Running";
  async function documentWithDiagnostic(running: Awaited<ReturnType<typeof host>>) {
    await until(() => runningStatus(running), running.logs.join("\n"));
    const file = path.join(directory, "example.kio");
    fs.writeFileSync(file, "module example;\n");
    const document = await vscode.workspace.openTextDocument(file);
    await until(() => vscode.languages.getDiagnostics(document.uri).some((diagnostic) => diagnostic.source === "availability-fixture"), "recovered server publishes diagnostics");
    return document;
  }

  test("repeated crashes exhaust the client budget and stop notifying", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "crash" }));
    const running = await host(fixture, control);
    const attempts = () => fs.existsSync(`${control}.starts`) ? fs.readFileSync(`${control}.starts`, "utf8").trim().split("\n").length : 0;
    try {
      await until(() => running.notifications.length === 1, "crash restart budget exhausted");
      assert.strictEqual(attempts(), 5, "the default client budget must bound restarts");
      assert.strictEqual(running.notifications.length, 1, JSON.stringify(running.notifications));
      assert.ok(running.notifications[0].actions.includes("Retry"));
      fs.writeFileSync(control, JSON.stringify({ mode: "healthy" }));
      await running.commands.get("kio.restartLanguageServer")!();
      await documentWithDiagnostic(running);
      assert.strictEqual(attempts(), 6);
    } finally { await running.close(); }
  });

  test("transport closure reaps each still-running server before restarting", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "transport-close-alive" }));
    const running = await host(fixture, control);
    try {
      await until(() => running.notifications.length === 1, "transport restart budget exhausted");
      const pids = fs.readFileSync(`${control}.starts`, "utf8").trim().split("\n").map(Number);
      assert.strictEqual(pids.length, 5);
      for (const pid of pids) assert.throws(() => process.kill(pid, 0), "closed transports do not leave live server processes");
    } finally {
      try { await running.close(); }
      finally {
        if (fs.existsSync(`${control}.starts`)) {
          for (const pid of fs.readFileSync(`${control}.starts`, "utf8").trim().split("\n").map(Number)) {
            try { process.kill(pid, "SIGKILL"); } catch { /* Already reaped. */ }
          }
        }
      }
    }
  });

  test("an invalid setting does not fall back and configuration repair restores diagnostics", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "healthy", version: "97.3.1-dev" }));
    const running = await host(path.join(directory, "missing-kio"), control, { environment: { KIO_BIN: fixture } });
    try {
      await until(() => running.notifications.length === 1, "missing setting notification");
      assert.match(running.notifications[0].message, /kio\.server\.path/);
      assert.ok(!fs.existsSync(`${control}.starts`), "explicit invalid path keeps its precedence");
      await running.configure(fixture);
      await documentWithDiagnostic(running);
      assert.strictEqual(running.notifications.length, 1, "a different compiler version and ordinary diagnostics do not warn");
      assert.match(running.statuses[0].detail ?? "", /97\.3\.1-dev/);
    } finally { await running.close(); }
  });

  test("missing compiler dismissal persists while status and retry stay available", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "healthy" }));
    const running = await host(null, control, { environment: { PATH: "", KIO_BIN: "" }, choice: "Don't show again" });
    try {
      await until(() => running.state.get("kio.serverNotFound.dismissed") === true, "workspace dismissal recorded");
      assert.strictEqual(running.notifications[0].severity, "Information");
      await running.commands.get("kio.restartLanguageServer")!();
      assert.strictEqual(running.notifications.length, 1);
      assert.strictEqual(running.statuses[0].severity, vscode.LanguageStatusSeverity.Error);
      assert.ok(running.commands.has("kio.showLanguageServerOutput"));
    } finally { await running.close(); }
  });

  test("missing-compiler dismissal does not hide subsequent server failures", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "startup-exit" }));
    const running = await host(fixture, control, { state: new Map([["kio.serverNotFound.dismissed", true]]) });
    try { await until(() => running.notifications.length === 1, "startup failure remains visible"); }
    finally { await running.close(); }
  });

  test("an invalid environment override does not fall back to PATH", async () => {
    const running = await host(null, control, { environment: { KIO_BIN: path.join(directory, "missing-kio") } });
    try {
      await until(() => running.notifications.length === 1, "environment override failure");
      assert.match(running.notifications[0].message, /KIO_BIN/);
      assert.strictEqual(running.notifications[0].severity, "Error");
    } finally { await running.close(); }
  });

  test("a compiler without the lsp command is rejected before server startup", async () => {
    const stale = path.join(vscode.extensions.getExtension("kio-lang.kio")!.extensionPath, "test/fixtures/stale-kio.js");
    const marker = path.join(directory, "spawned");
    const running = await host(stale, control, { environment: { VSCODE_KIO_STALE_MARKER: marker } });
    try {
      await until(() => running.notifications.length === 1, "missing command failure");
      assert.match(running.notifications[0].message, /no `lsp` command/);
      assert.strictEqual(running.statuses[0].text, "Kio: Language services unavailable");
      assert.strictEqual(running.statuses[0].busy, false);
      assert.ok(!fs.existsSync(marker));
      assert.ok(running.notifications[0].actions.includes("Retry"));
    } finally { await running.close(); }
  });

  if (process.platform !== "win32") test("a non-executable compiler gives an actionable failure", async () => {
    const unavailable = path.join(directory, "not-executable");
    fs.writeFileSync(unavailable, "not executable", { mode: 0o600 });
    const running = await host(unavailable, control);
    try {
      await until(() => running.notifications.length === 1, "permission failure");
      assert.match(running.notifications[0].message, /not executable/);
      assert.ok(running.notifications[0].actions.includes("Show Output"));
    } finally { await running.close(); }
  });

  for (const mode of ["probe-failure", "probe-timeout", "startup-exit", "initialize-error", "initialize-timeout", "transport-error", "crash-then-hang"]) {
    test(`${mode} is bounded, actionable and recoverable`, async () => {
      fs.writeFileSync(control, JSON.stringify({ mode }));
      const running = await host(fixture, control);
      try {
        await until(() => running.notifications.length > 0, `${mode} notification; ${running.logs.join("\n")}`, 14_000);
        await delay(100);
        assert.strictEqual(running.notifications.length, 1, JSON.stringify(running.notifications));
        assert.ok(running.notifications[0].actions.includes("Retry"));
        assert.strictEqual(running.statuses[0].busy, false);
        assert.ok(!running.logs.join("\n").includes("user:secret"), "credentials are removed from surfaced errors");
        fs.writeFileSync(control, JSON.stringify({ mode: "healthy" }));
        await running.commands.get("kio.restartLanguageServer")!();
        await documentWithDiagnostic(running);
      } finally { await running.close(); }
    });
  }

  test("one crash recovers quietly", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "crash-once" }));
    const running = await host(fixture, control);
    try {
      await until(() => running.logs.filter((line) => line.includes("language client started")).length === 2, "automatic recovery");
      await documentWithDiagnostic(running);
      assert.strictEqual(running.notifications.length, 0);
    } finally { await running.close(); }
  });

  test("rapid setting changes cancel a stale probe without an alert", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "probe-timeout" }));
    const running = await host(fixture, control);
    try {
      await until(() => fs.existsSync(`${control}.probes`), "probe started");
      fs.writeFileSync(control, JSON.stringify({ mode: "healthy" }));
      await running.configure(path.join(directory, "discarded-setting"));
      await running.configure(fixture);
      await documentWithDiagnostic(running);
      assert.strictEqual(running.notifications.length, 0);
      assert.strictEqual(fs.readFileSync(`${control}.starts`, "utf8").trim().split("\n").length, 1);
    } finally { await running.close(); }
  });

  test("configuration repair cancels an automatic restart still initializing", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "crash-then-hang" }));
    const running = await host(fixture, control);
    try {
      const initializing = () => fs.existsSync(`${control}.initializes`)
        ? fs.readFileSync(`${control}.initializes`, "utf8").trim().split("\n").map(Number) : [];
      await until(() => initializing().length === 2, "automatic restart is waiting for initialization");
      const obsolete = initializing();
      fs.writeFileSync(control, JSON.stringify({ mode: "healthy" }));
      await running.configure(fixture);
      await documentWithDiagnostic(running);
      for (const pid of obsolete) assert.throws(() => process.kill(pid, 0), "configuration replacement reaps obsolete attempts");
      assert.strictEqual(running.notifications.length, 0, "cancelled startup does not alert after recovery");
    } finally { await running.close(); }
  });

  for (const mode of ["probe-timeout", "initialize-timeout"]) {
    test(`deactivation reaps ${mode}`, async () => {
      fs.writeFileSync(control, JSON.stringify({ mode }));
      const running = await host(fixture, control, { holdStartupDeadlines: true });
      try {
        const marker = `${control}.${mode === "probe-timeout" ? "probes" : "initializes"}`;
        await until(() => fs.existsSync(marker), "child started");
        const childPid = Number(fs.readFileSync(marker, "utf8").trim());
        assert.doesNotThrow(() => process.kill(childPid, 0), "startup remains pending before deactivation");
        if (mode === "probe-timeout") {
          assert.strictEqual(running.probeSignals.at(-1)?.aborted, false);
        } else {
          assert.strictEqual(running.heldDeadlines.size, 1, "initialization deadline is held");
        }
        let guard: ReturnType<typeof setTimeout> | undefined;
        try {
          await Promise.race([
            running.close(),
            new Promise<never>((_, reject) => {
              guard = setTimeout(() => reject(new Error("deactivation did not cancel pending startup")), 15_000);
            }),
          ]);
        } finally { clearTimeout(guard); }
        assert.throws(() => process.kill(childPid, 0), "shutdown waits for the child to exit");
        if (mode === "probe-timeout") {
          assert.strictEqual(running.probeSignals.at(-1)?.aborted, true, "shutdown aborts the pending probe");
        } else {
          assert.strictEqual(running.heldDeadlines.size, 0, "shutdown cancels the held initialization deadline");
        }
        assert.strictEqual(running.notifications.length, 0);
      } finally {
        for (const child of running.startupChildren) {
          if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
        }
        await running.close();
      }
    });
  }

  for (const mode of ["request-error", "request-rejected"]) {
    test(`${mode} has truthful status without notification spam`, async () => {
      fs.writeFileSync(control, JSON.stringify({ mode }));
      const running = await host(fixture, control);
      try {
        const document = await documentWithDiagnostic(running);
        for (let attempt = 0; attempt < 3; attempt++) {
          await vscode.commands.executeCommand("vscode.executeHoverProvider", document.uri, new vscode.Position(0, 1)).then(() => {}, () => {});
        }
        assert.strictEqual(running.notifications.length, 0);
        assert.strictEqual(running.statuses[0].severity, mode === "request-error" ? vscode.LanguageStatusSeverity.Warning : vscode.LanguageStatusSeverity.Information);
      } finally { await running.close(); }
    });
  }

  test("request failure remains visible after recovery", async () => {
    fs.writeFileSync(control, JSON.stringify({ mode: "request-error-crash" }));
    const running = await host(fixture, control);
    try {
      const document = await documentWithDiagnostic(running);
      const hover = () => vscode.commands.executeCommand("vscode.executeHoverProvider", document.uri, new vscode.Position(0, 1)).then(() => {}, () => {});
      await hover();
      assert.strictEqual(running.statuses[0].severity, vscode.LanguageStatusSeverity.Warning);
      await until(() => running.logs.filter((line) => line.includes("language client started")).length === 2, "automatic recovery after failed request");
      assert.strictEqual(running.statuses[0].severity, vscode.LanguageStatusSeverity.Information);
      const starts = fs.readFileSync(`${control}.starts`, "utf8").trim().split("\n");
      assert.strictEqual(starts.length, 2);
      // Running precedes provider registration; the asynchronous diagnostic
      // response follows the client's synchronous feature initialization.
      await until(() => running.diagnostics.get(document.uri.toString())?.some((diagnostic) => diagnostic.source === "availability-fixture" && diagnostic.code === Number(starts[1])) ?? false, "recovered server republishes diagnostics after provider registration");
      await hover();
      assert.strictEqual(running.statuses[0].severity, vscode.LanguageStatusSeverity.Warning);
      const hovers = fs.readFileSync(`${control}.hovers`, "utf8").trim().split("\n");
      assert.deepStrictEqual(hovers, starts, "each server process handles its failed hover request");
      assert.strictEqual(running.notifications.length, 0);
    } finally { await running.close(); }
  });
});

if (process.env.VSCODE_KIO_E2E_MODE === "real")
suite("Kio extension — real compiler recovery", () => {
  test("configuration repair and explicit restart both restore real type diagnostics", async () => {
    const workspace = process.env.VSCODE_KIO_TEST_WORKSPACE!;
    const binary = process.env.KIO_E2E_SERVER!;
    const running = await host(path.join(workspace, "missing-kio"), "");
    try {
      await until(() => running.notifications.length === 1, "invalid compiler path notification");
      const document = await vscode.workspace.openTextDocument(path.join(workspace, "lsp-pkg/main.kio"));
      assert.strictEqual(running.diagnostics.size, 0, "the unavailable client cannot analyze the document");
      await running.configure(binary);
      const recovered = () => running.diagnostics.get(document.uri.toString())?.some((diagnostic) =>
        diagnostic.source === "kio" && diagnostic.code === 14 && /type mismatch/.test(diagnostic.message)) ?? false;
      await until(recovered, "configured compiler publishes its type error", 20_000);
      running.diagnostics.clear();
      await running.commands.get("kio.restartLanguageServer")!();
      await until(recovered, "restarted compiler republishes its type error", 20_000);
      assert.strictEqual(running.notifications.length, 1, "ordinary Kio source errors do not create availability alerts");
    } finally { await running.close(); }
  });
});
