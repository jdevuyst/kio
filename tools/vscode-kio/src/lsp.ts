import * as vscode from "vscode";
import { CloseAction, ErrorAction, ErrorCodes, LanguageClient, ResponseError, State } from "vscode-languageclient/node";
import { ChildProcess, execFile, spawn } from "node:child_process";
import * as fs from "node:fs";

const RESTART = "kio.restartLanguageServer";
const OUTPUT = "kio.showLanguageServerOutput";
const MISSING_DISMISSED = "kio.serverNotFound.dismissed";

type Resolution =
  | { kind: "found"; path: string; source: string }
  | { kind: "missing"; reason: string; configured: boolean };

function safeText(value: string): string {
  return value.replace(/(https?:\/\/)[^\s/@]+@/gi, "$1[redacted]@").replace(/[\x00-\x08\x0b\x0c\x0e-\x1f]/g, "");
}

function errorText(error: unknown): string {
  const candidate = error as { message?: string; code?: string };
  return safeText(`${candidate?.code ?? ""} ${candidate?.message ?? String(error)}`.trim());
}

function isFile(candidate: string): boolean {
  try { return fs.statSync(candidate).isFile(); } catch { return false; }
}

function resolveServerPath(): Resolution {
  const setting = vscode.workspace.getConfiguration("kio").get<string | null>("server.path");
  for (const [source, candidate] of [["kio.server.path", setting], ["KIO_BIN", process.env.KIO_BIN]]) {
    if (!candidate) continue;
    return isFile(candidate)
      ? { kind: "found", path: candidate, source: source! }
      : { kind: "missing", configured: true, reason: `${source} does not name an existing executable file.` };
  }
  const extensions = process.platform === "win32" ? (process.env.PATHEXT ?? ".EXE;.CMD;.BAT").split(";") : [""];
  for (const directory of (process.env.PATH ?? "").split(process.platform === "win32" ? ";" : ":")) {
    if (!directory) continue;
    for (const extension of extensions) {
      const candidate = `${directory}/kio${extension}`;
      if (isFile(candidate)) return { kind: "found", path: candidate, source: "PATH" };
    }
  }
  return { kind: "missing", configured: false, reason: "`kio` was not found on PATH in this environment." };
}

async function probeServer(binary: string, signal: AbortSignal): Promise<void> {
  let closed = Promise.resolve();
  const result = new Promise<void>((resolve, reject) => {
    const child = execFile(binary, ["--help"], {
      encoding: "utf8", env: process.env, timeout: 5_000, killSignal: "SIGKILL", maxBuffer: 64 * 1024, signal,
    }, (error, stdout, stderr) => {
      if (error) {
        const code = (error as NodeJS.ErrnoException).code;
        const reason = error.killed ? "The compiler probe timed out."
          : code === "EACCES" ? "The selected compiler is not executable."
          : code === "ENOEXEC" ? "The selected compiler cannot run on this platform."
          : `The compiler probe failed (${code ?? "unknown error"}).`;
        reject(new Error(reason));
      } else if (!/^\s+lsp\b/m.test(`${stdout}\n${stderr}`)) {
        reject(new Error("The selected compiler has no `lsp` command. Install a Kio build with language-server support."));
      } else resolve();
    });
    closed = new Promise<void>((resolve) => child.once("close", () => resolve()));
  });
  try { await result; } finally { await closed; }
}

// Startup connections cannot be stopped by the library until initialization
// finishes. Owning the child lets cancellation and deadlines reap that state.
class KioClient extends LanguageClient {
  stopping = false;
  terminateServer: () => Promise<void> = async () => {};
  failedStart: (error: unknown) => void = () => {};
  failedRequest: () => void = () => {};

  override async start(): Promise<void> {
    this.stopping = false;
    // A close during initialization clears the library's cached start promise.
    // Its coalescing start API lets us observe that promise before it is cleared.
    const starting = super.start();
    const initialized = super.start();
    try { await Promise.all([starting, initialized]); }
    catch (error) { this.failedStart(error); throw error; }
  }

  override async stop(timeout?: number): Promise<void> {
    this.stopping = true;
    try {
      if (this.state !== State.Starting) await super.stop(timeout);
    } finally { await this.terminateServer(); }
  }

  override error(message: string, data?: unknown): void {
    super.error(safeText(message), data === undefined ? undefined : errorText(data), false);
    if (data instanceof ResponseError && data.code >= ErrorCodes.ParseError && data.code <= ErrorCodes.InvalidRequest) this.failedRequest();
  }
}

export type LspDisposable = vscode.Disposable & { stop(): Promise<void> };

class LanguageServer implements LspDisposable {
  private client?: KioClient;
  private clientState?: vscode.Disposable;
  private probe?: AbortController;
  private sequence = 0;
  private stopped = false;
  private pending = Promise.resolve();
  private notified = new Set<string>();
  private dismissed = new Set<string>();
  private subscriptions: vscode.Disposable[] = [];
  private output: vscode.OutputChannel;

  constructor(private context: vscode.ExtensionContext, private status: vscode.LanguageStatusItem) {
    this.output = vscode.window.createOutputChannel("Kio LSP");
    this.subscriptions.push(this.output,
      vscode.commands.registerCommand(RESTART, () => this.restart()),
      vscode.commands.registerCommand(OUTPUT, () => this.output.show(true)),
      vscode.workspace.onDidChangeConfiguration((event) => {
        if (event.affectsConfiguration("kio.server.path")) void this.restart();
      }),
    );
    void vscode.commands.executeCommand("setContext", "kio.languageServerSupported", true);
    void this.restart();
  }

  private log(message: string): void { this.output.appendLine(`lsp: ${safeText(message)}`); }

  private show(text: string, detail: string, severity = vscode.LanguageStatusSeverity.Information, busy = false): void {
    this.status.text = text;
    this.status.detail = safeText(detail);
    this.status.severity = severity;
    this.status.busy = busy;
    this.status.command = { command: severity === vscode.LanguageStatusSeverity.Error ? RESTART : OUTPUT,
      title: severity === vscode.LanguageStatusSeverity.Error ? "Retry" : "Show Output" };
  }

  private failure(category: string, detail: string, missing = false): void {
    this.log(detail);
    if (this.notified.has(category)) return;
    this.show("Kio: Language services unavailable", detail, vscode.LanguageStatusSeverity.Error);
    if (this.dismissed.has(category) ||
      (missing && this.context.workspaceState.get<boolean>(MISSING_DISMISSED))) return;
    this.notified.add(category);
    const sequence = this.sequence;
    const message = `Kio language services are unavailable. ${safeText(detail)} Highlighting remains available.`;
    const actions = missing ? ["Install Instructions", "Open Settings", "Don't show again"]
      : category === "configuration" ? ["Open Settings", "Retry", "Show Output", "Dismiss for Session"]
      : ["Retry", "Show Output", "Dismiss for Session"];
    const notify = missing ? vscode.window.showInformationMessage : vscode.window.showErrorMessage;
    void notify(message, ...actions).then((choice) => {
      if (this.stopped || sequence !== this.sequence) return;
      if (choice === "Retry") void this.restart();
      if (choice === "Show Output") this.output.show(true);
      if (choice === "Open Settings") void vscode.commands.executeCommand("workbench.action.openSettings", "kio.server.path");
      if (choice === "Install Instructions") void vscode.env.openExternal(vscode.Uri.parse("https://jdevuyst.github.io/kio/"));
      if (choice === "Don't show again") void this.context.workspaceState.update(MISSING_DISMISSED, true);
      if (choice === "Dismiss for Session") this.dismissed.add(category);
    });
  }

  restart(): Promise<void> {
    if (this.stopped) return this.pending;
    const sequence = ++this.sequence;
    this.probe?.abort();
    if (this.client?.state === State.Starting) void this.client.stop().catch(() => {});
    this.pending = this.pending.then(async () => {
      if (this.stopped || sequence !== this.sequence) return;
      await this.disposeClient();
      if (this.stopped || sequence !== this.sequence) return;
      this.notified.clear();
      await this.start(sequence);
    }).catch((error) => {
      if (!this.stopped && sequence === this.sequence) this.failure("startup", errorText(error));
    });
    return this.pending;
  }

  private async start(sequence: number): Promise<void> {
    this.show("Kio: Starting…", "Locating the compiler and starting the language server.", undefined, true);
    const resolution = resolveServerPath();
    if (resolution.kind === "missing") {
      this.failure("configuration", resolution.reason, !resolution.configured);
      return;
    }
    this.log(`using ${resolution.source}: ${resolution.path}`);
    const probe = new AbortController();
    this.probe = probe;
    try { await probeServer(resolution.path, probe.signal); }
    finally { if (this.probe === probe) this.probe = undefined; }
    if (this.stopped || sequence !== this.sequence) return;

    let attemptRunning = false;
    let server: { child: ChildProcess; exited: Promise<void> } | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const active = () => !this.stopped && sequence === this.sequence && this.client === client;
    const clearDeadline = () => { clearTimeout(timer); timer = undefined; };
    const client: KioClient = new KioClient("kio", "Kio Language Server", async () => {
      if (!active() || client.stopping) throw new Error("Language-server startup was cancelled.");
      attemptRunning = false;
      const child = spawn(resolution.path, ["lsp"], { env: process.env, stdio: "pipe" });
      const exited = new Promise<void>((resolve) => {
        child.once("exit", (code, signal) => { this.log(`server exited (${signal ?? code})`); resolve(); });
        child.once("error", () => resolve());
      });
      server = { child, exited };
      child.stderr!.setEncoding("utf8");
      child.stderr!.on("data", (text: string) => this.output.append(safeText(text)));
      timer = setTimeout(() => {
        if (active() && !attemptRunning) {
          this.failure("startup", "The language server did not finish starting within 10 seconds. See Kio LSP output.");
          child.kill("SIGKILL");
        }
      }, 10_000);
      await new Promise<void>((resolve, reject) => { child.once("spawn", resolve); child.once("error", reject); });
      return { reader: child.stdout!, writer: child.stdin! };
    }, {
      documentSelector: [{ scheme: "file", language: "kio" }],
      outputChannel: this.output, traceOutputChannel: this.output,
      initializationFailedHandler: (error) => {
        if (active() && !client.stopping) this.failure("startup", `The language server could not initialize: ${errorText(error)}. See Kio LSP output.`);
        return false;
      },
      errorHandler: {
        error: async (error, message, count) => {
          const result = await budget.error(error, message, count);
          if (active() && result.action === ErrorAction.Shutdown) this.failure("connection", "The language-server connection failed repeatedly. See Kio LSP output.");
          return { ...result, handled: true };
        },
        closed: async () => {
          // A closed transport does not imply that its process has exited.
          // Reap the old attempt before the client can create its replacement.
          await client.terminateServer();
          if (!active() || client.stopping) return { action: CloseAction.DoNotRestart, handled: true };
          if (!attemptRunning) {
            this.failure("startup", "The language server exited before it finished starting. See Kio LSP output.");
            return { action: CloseAction.DoNotRestart, handled: true };
          }
          const result = await budget.closed();
          if (!active() || client.stopping) return { action: CloseAction.DoNotRestart, handled: true };
          if (result.action === CloseAction.Restart) this.show("Kio: Restarting…", "The language server exited unexpectedly.", undefined, true);
          else this.failure("crashes", "The language server stopped after repeated failures. See Kio LSP output.");
          return { ...result, handled: true };
        },
      },
    });
    const budget = client.createDefaultErrorHandler();
    this.client = client;
    client.terminateServer = async () => {
      clearDeadline();
      if (server) {
        if (server.child.exitCode === null && server.child.signalCode === null) server.child.kill("SIGKILL");
        await server.exited;
      }
    };
    client.failedStart = (error) => {
      clearDeadline();
      if (active() && !client.stopping) this.failure("startup", `Could not start the language server: ${errorText(error)}. See Kio LSP output.`);
    };
    client.failedRequest = () => {
      if (!active() || !attemptRunning) return;
      this.show("Kio: Request failed", "The server is connected, but a language request failed. See Kio LSP output.", vscode.LanguageStatusSeverity.Warning);
    };
    this.clientState = client.onDidChangeState((event) => {
      if (!active()) return;
      if (event.newState === State.Running) {
        attemptRunning = true;
        clearDeadline();
        const version = client.initializeResult?.serverInfo?.version;
        this.show("Kio: Running", `Language server connected${version ? ` (${safeText(version)})` : ""}.`);
        this.log("language client started");
      }
    });
    this.log(`starting language client (server: ${resolution.path}, source: ${resolution.source})`);
    try { await client.start(); }
    catch (error) {
      if (active()) this.failure("startup", `Could not start the language server: ${errorText(error)}. See Kio LSP output.`);
      await client.dispose().catch(() => {});
    }
  }

  private async disposeClient(): Promise<void> {
    const client = this.client;
    this.client = undefined;
    this.clientState?.dispose();
    this.clientState = undefined;
    if (client) await client.dispose().catch((error) => this.log(`stopping server: ${errorText(error)}`));
  }

  stop(): Promise<void> {
    if (this.stopped) return this.pending;
    this.stopped = true;
    ++this.sequence;
    this.probe?.abort();
    if (this.client?.state === State.Starting) void this.client.stop().catch(() => {});
    this.pending = this.pending.then(() => this.disposeClient()).finally(() => {
      for (const subscription of this.subscriptions.splice(0)) subscription.dispose();
      void vscode.commands.executeCommand("setContext", "kio.languageServerSupported", false);
    });
    return this.pending;
  }

  dispose(): void { void this.stop(); }
}

export function startLanguageClient(context: vscode.ExtensionContext, status: vscode.LanguageStatusItem): LspDisposable {
  return new LanguageServer(context, status);
}
