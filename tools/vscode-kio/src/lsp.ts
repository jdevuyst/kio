// Language Server Protocol client wiring for local and remote Node hosts.
//
// Loaded lazily from `extension.ts` via `await import("./lsp")` when
// the extension host runs Node. Browser-only hosts use the grammar
// layers without evaluating the client's Node dependencies.
//
// Server discovery follows three rules in order:
//
//   1. The `kio.server.path` user setting (if non-empty and pointing
//      at an existing file). Surfaced as a notification when set but
//      invalid, so the user sees the typo rather than silent fallback.
//   2. The `KIO_BIN` environment variable. Same validity check.
//   3. `kio` looked up on `PATH` (walk `PATH` directories looking
//      for the file; no child-process spawn, just `fs.statSync`).
//
// If none resolves to a runnable binary, the client is not started
// and the user gets a one-time, dismissable information notification
// (persisted via workspace state) inviting them to install `kio` or
// point the setting at it. Highlighting (TextMate + tree-sitter)
// keeps working — the LSP layer is purely additive.
//
// The construction is intentionally minimal: stdio transport,
// `{scheme:"file", language:"kio"}` document filter, trace level
// driven by the `kio.trace.server` setting. Server-capability
// negotiation happens through `initialize`; later sessions in this
// track add hover / goto / completion etc. without further changes
// here.

import * as vscode from "vscode";
import {
  CloseAction,
  CloseHandlerResult,
  ErrorAction,
  ErrorHandlerResult,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  State,
} from "vscode-languageclient/node";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs";

const NOTIFICATION_DISMISSED_KEY = "kio.serverNotFound.dismissed";

/// Resolve the path the extension should spawn for `kio lsp`. Returns
/// `{kind: "found", path}` when one of the three discovery rules
/// succeeded, or `{kind: "missing"}` otherwise. Errors during
/// resolution (a configured path that doesn't exist) are folded into
/// `missing` so the caller can branch on a single bit.
export type Resolution =
  | { kind: "found"; path: string; source: ResolutionSource }
  | { kind: "missing"; reason: string };

export type ResolutionSource = "setting" | "env" | "path";

/// Disposable shape returned by [`startLanguageClient`]. Adds an
/// async `stop` method on top of the standard `vscode.Disposable` so
/// `deactivate` can wait for the LSP shutdown handshake before the
/// extension host moves on.
export type LspDisposable = vscode.Disposable & {
  stop: () => Promise<void>;
};

export function resolveServerPath(
  output: vscode.OutputChannel,
): Resolution {
  const cfg = vscode.workspace.getConfiguration("kio");
  const configured = cfg.get<string | null>("server.path");
  if (configured !== null && configured !== undefined && configured !== "") {
    if (isExecutableFile(configured)) {
      output.appendLine(
        `lsp: using server path from kio.server.path setting: ${configured}`,
      );
      return { kind: "found", path: configured, source: "setting" };
    }
    return {
      kind: "missing",
      reason: `kio.server.path setting points at ${configured}, but no executable file exists there`,
    };
  }

  const envVar = process.env.KIO_BIN;
  if (envVar !== undefined && envVar !== "") {
    if (isExecutableFile(envVar)) {
      output.appendLine(`lsp: using server path from KIO_BIN: ${envVar}`);
      return { kind: "found", path: envVar, source: "env" };
    }
    return {
      kind: "missing",
      reason: `KIO_BIN env var points at ${envVar}, but no executable file exists there`,
    };
  }

  const onPath = findOnPath("kio");
  if (onPath !== null) {
    output.appendLine(`lsp: using server path from PATH: ${onPath}`);
    return { kind: "found", path: onPath, source: "path" };
  }
  return {
    kind: "missing",
    reason: "`kio` not found on PATH, KIO_BIN unset, kio.server.path unset",
  };
}

function isExecutableFile(p: string): boolean {
  try {
    const stat = fs.statSync(p);
    return stat.isFile();
  } catch {
    return false;
  }
}

/// Walk `PATH` looking for `name` (with platform-appropriate
/// executable extensions on Windows). Returns the first matching
/// absolute path, or null if none found. Avoids a `child_process`
/// spawn — synchronous filesystem checks are cheaper and don't
/// depend on the candidate binary being able to run.
function findOnPath(name: string): string | null {
  const pathVar = process.env.PATH ?? "";
  const sep = process.platform === "win32" ? ";" : ":";
  const exts =
    process.platform === "win32"
      ? (process.env.PATHEXT ?? ".EXE;.CMD;.BAT").split(";")
      : [""];
  for (const dir of pathVar.split(sep)) {
    if (dir === "") continue;
    for (const ext of exts) {
      const candidate = `${dir}/${name}${ext}`;
      if (isExecutableFile(candidate)) return candidate;
    }
  }
  return null;
}

/// Outcome of [`probeServer`]: whether the resolved binary can serve
/// `kio lsp` at all. `unusable` carries a user-facing `reason` and the
/// `hint` that tells them what to do about it.
export type ServerCheck =
  | { kind: "ok" }
  | { kind: "unusable"; reason: string; hint: string };

/// Ask the resolved binary whether it can serve `kio lsp`, by reading
/// the subcommand list out of its `--help`.
///
/// Resolution proves a file exists; it does not prove the file is a
/// `kio` that speaks LSP. Three ways it isn't: a binary too old to
/// have the subcommand (a stale `target/release/kio` outlives any
/// `git pull` — the directory is git-ignored and nothing rebuilds it),
/// a `kio` built without the `lsp` cargo feature, and a binary for a
/// different platform (a host-built artifact seen through a container
/// mount). Each one fails the same way every time, so letting the
/// language client discover it means an identical crash repeated until
/// the restart budget runs out — five stack traces where one sentence
/// would do.
///
/// `kio --help` lists a subcommand only when the feature that
/// implements it is compiled in, which makes the help text the
/// binary's own account of what it can do.
export function probeServer(binPath: string): ServerCheck {
  const probe = spawnSync(binPath, ["--help"], {
    encoding: "utf8",
    timeout: 5_000,
  });

  if (probe.error !== undefined) {
    const code = (probe.error as NodeJS.ErrnoException).code;
    const hint =
      code === "ENOEXEC"
        ? "That file is not runnable on this platform — a binary built on the host cannot run inside a container. Rebuild it here."
        : "Rebuild it (`cargo build --release` in `kio-rs`) or point `kio.server.path` at a working `kio`.";
    return {
      kind: "unusable",
      reason: `could not run ${binPath}: ${probe.error.message}`,
      hint,
    };
  }

  const help = `${probe.stdout ?? ""}${probe.stderr ?? ""}`;
  if (probe.status !== 0 || !/^\s+lsp\b/m.test(help)) {
    return {
      kind: "unusable",
      reason: `${binPath} has no \`lsp\` subcommand`,
      hint:
        "It is stale, or built without the `lsp` cargo feature. Rebuild it (`cargo build --release` in `kio-rs`) or point `kio.server.path` at a `kio` that has `kio lsp`.",
    };
  }

  return { kind: "ok" };
}

/// Start the language client. Returns a disposable that stops the
/// client when fired (e.g. from `context.subscriptions`); returns
/// `undefined` when the server couldn't be located or cannot serve
/// LSP, and the user has been notified.
export async function startLanguageClient(
  context: vscode.ExtensionContext,
  output: vscode.OutputChannel,
): Promise<LspDisposable | undefined> {
  const resolution = resolveServerPath(output);
  if (resolution.kind === "missing") {
    output.appendLine(`lsp: ${resolution.reason}`);
    maybeNotifyMissing(context);
    return undefined;
  }

  const check = probeServer(resolution.path);
  if (check.kind === "unusable") {
    output.appendLine(`lsp: ${check.reason}\nlsp: ${check.hint}`);
    void vscode.window.showWarningMessage(
      `Kio: ${check.reason}. ${check.hint}`,
    );
    return undefined;
  }

  const serverOptions: ServerOptions = {
    command: resolution.path,
    args: ["lsp"],
    // Stdio is the LSP scaffold's only transport — see specs/cli.md
    // § `kio lsp`. The default `Executable` shape uses stdio
    // implicitly.
    options: {
      env: process.env,
    },
  };

  // A dedicated output channel for LSP traffic. Distinct from the
  // existing "Kio" channel used by the highlighting layer so trace
  // output doesn't get mixed in with grammar-loading diagnostics.
  const lspOutput = vscode.window.createOutputChannel("Kio LSP");
  context.subscriptions.push(lspOutput);

  // A server that dies before it ever reaches Running dies the same
  // way on every restart, so the default handler's budget converts one
  // failure into a cascade of identical ones and buries the first —
  // the only informative — error. Restart only a server that has
  // served at least once.
  let everRunning = false;

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "kio" }],
    // `outputChannel` receives server-side log messages
    // (`window/logMessage`); `traceOutputChannel` receives the raw
    // JSON-RPC trace when `kio.trace.server` is `messages` /
    // `verbose`. Pointing both at the same channel keeps everything
    // in one pane.
    outputChannel: lspOutput,
    traceOutputChannel: lspOutput,
    errorHandler: {
      error: (): ErrorHandlerResult => ({ action: ErrorAction.Continue }),
      closed: (): CloseHandlerResult =>
        everRunning
          ? { action: CloseAction.Restart }
          : {
              action: CloseAction.DoNotRestart,
              message: `The Kio language server (${resolution.path}) exited before it finished starting. See the "Kio LSP" output channel.`,
            },
    },
  };

  // Client id `"kio"` is also the prefix vscode-languageclient uses
  // to read the trace setting (`kio.trace.server`) and to watch for
  // changes via `onDidChangeConfiguration`. Aligning the id with the
  // `kio.*` configuration namespace declared in package.json makes
  // the trace-level wiring automatic.
  const client = new LanguageClient(
    "kio",
    "Kio Language Server",
    serverOptions,
    clientOptions,
  );

  context.subscriptions.push(
    client.onDidChangeState((e) => {
      if (e.newState === State.Running) everRunning = true;
    }),
  );

  output.appendLine(
    `lsp: starting language client (server: ${resolution.path}, source: ${resolution.source})`,
  );
  try {
    await client.start();
    output.appendLine("lsp: language client started");
  } catch (err) {
    output.appendLine(`lsp: failed to start language client:\n${formatErr(err)}`);
    // The client object may be in a partially-initialized state;
    // best-effort stop so any spawned process is reaped.
    await client.stop().catch(() => {
      // Ignore — we're already in an error path; double-faulting on
      // shutdown would mask the original failure.
    });
    return undefined;
  }

  return {
    dispose: () => {
      // `stop()` is async; VS Code's own `Disposable.dispose` shape
      // is synchronous, so dropping this on the floor via
      // `context.subscriptions` would race the extension host into
      // tearing down before the LSP `shutdown` / `exit` handshake
      // completes. `deactivate` in extension.ts holds onto the
      // disposable returned from `startLanguageClient` and invokes
      // `stop` directly with `await` so the child reaps cleanly on
      // editor reload.
      void client.stop();
    },
    stop: async (): Promise<void> => {
      await client.stop();
    },
  };
}

function maybeNotifyMissing(context: vscode.ExtensionContext): void {
  if (context.workspaceState.get<boolean>(NOTIFICATION_DISMISSED_KEY)) {
    return;
  }
  const installAction = "How to install `kio`";
  void vscode.window
    .showInformationMessage(
      "Kio: language server (`kio`) not found. Diagnostics are disabled; highlighting still works. Set `kio.server.path` or install the `kio` CLI.",
      installAction,
      "Don't show again",
    )
    .then((choice) => {
      if (choice === installAction) {
        void vscode.env.openExternal(
          vscode.Uri.parse("https://github.com/jdevuyst/kio"),
        );
      } else if (choice === "Don't show again") {
        void context.workspaceState.update(NOTIFICATION_DISMISSED_KEY, true);
      }
    });
}

function formatErr(err: unknown): string {
  if (err instanceof Error) {
    return `${err.name}: ${err.message}\n${err.stack ?? "(no stack)"}`;
  }
  return String(err);
}
