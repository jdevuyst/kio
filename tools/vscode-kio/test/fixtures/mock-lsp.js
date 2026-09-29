#!/usr/bin/env node
// Mock LSP server for the VS Code extension's E2E test.
//
// Speaks Content-Length-framed JSON-RPC over stdin / stdout (the
// same wire protocol as `kio lsp`), but with a fixed canned response
// set. The test points `kio.server.path` at this script and asserts
// that:
//
//   - The client survives the `initialize` handshake (we advertise
//     `textDocumentSync.openClose = true` so the client subscribes
//     to didOpen / didClose).
//   - The client renders a diagnostic published in response to
//     `textDocument/didOpen` — proving the publishDiagnostics path
//     reaches the editor's gutter.
//
// Why a mock rather than the real `kio lsp`: the test asserts on
// the VS Code-side wiring (`vscode.languages.getDiagnostics(uri)`
// reflects what the server published), not on kio-rs's analyzer.
// Decoupling lets the test run without rebuilding the Rust binary
// and pins the assertion to a deterministic diagnostic shape.

"use strict";

// The extension probes the resolved binary with `--help` before
// spawning it, and reads the subcommand list to decide whether this is
// a `kio` that can serve LSP at all (see `probeServer` in `lsp.ts`).
// Standing in for `kio` means answering that question the way `kio`
// does — a mock that only speaks JSON-RPC would be rejected before the
// client ever starts.
if (process.argv.slice(2).includes("--help")) {
  process.stdout.write(
    "Usage: kio <subcommand> [args]\n\nSubcommands:\n  lsp                         Run the language server (mock).\n",
  );
  process.exit(0);
}

let buffer = Buffer.alloc(0);

process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  // Each iteration consumes one framed message if a complete one
  // is available; otherwise we wait for more bytes.
  while (true) {
    const message = tryParse();
    if (message === null) break;
    handle(message);
  }
});

process.stdin.on("end", () => {
  // Client closed stdin — exit cleanly.
  process.exit(0);
});

function tryParse() {
  const headerEnd = buffer.indexOf("\r\n\r\n");
  if (headerEnd < 0) return null;
  const header = buffer.slice(0, headerEnd).toString("utf8");
  const match = /Content-Length:\s*(\d+)/i.exec(header);
  if (match === null) {
    // Malformed header — drop and resync. The real client never
    // sends this; if it does, the test will catch it.
    buffer = buffer.slice(headerEnd + 4);
    return null;
  }
  const length = Number(match[1]);
  const bodyStart = headerEnd + 4;
  if (buffer.length < bodyStart + length) return null;
  const body = buffer.slice(bodyStart, bodyStart + length).toString("utf8");
  buffer = buffer.slice(bodyStart + length);
  try {
    return JSON.parse(body);
  } catch {
    return null;
  }
}

function send(message) {
  const body = JSON.stringify(message);
  const header = `Content-Length: ${Buffer.byteLength(body, "utf8")}\r\n\r\n`;
  process.stdout.write(header + body);
}

function handle(msg) {
  if (msg.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: msg.id,
      result: {
        capabilities: {
          textDocumentSync: { openClose: true, change: 0 },
        },
        serverInfo: { name: "mock-lsp", version: "0.0.0" },
      },
    });
    return;
  }
  if (msg.method === "initialized") {
    // No-op; the LSP spec lets the server skip any side-effects on
    // this notification, and the test asserts on a later didOpen.
    return;
  }
  if (msg.method === "textDocument/didOpen") {
    const uri = msg.params.textDocument.uri;
    // Publish a single diagnostic with a deterministic shape so the
    // test can assert by exact match.
    send({
      jsonrpc: "2.0",
      method: "textDocument/publishDiagnostics",
      params: {
        uri,
        diagnostics: [
          {
            range: {
              start: { line: 0, character: 0 },
              end: { line: 0, character: 6 },
            },
            severity: 1,
            source: "mock-lsp",
            message: "mock diagnostic from the test harness",
          },
        ],
      },
    });
    return;
  }
  if (msg.method === "shutdown") {
    send({ jsonrpc: "2.0", id: msg.id, result: null });
    return;
  }
  if (msg.method === "exit") {
    process.exit(0);
  }
  // Other requests / notifications: ignore. Anything that needs a
  // response stays unanswered, which the client surfaces as a
  // timeout — the test should never reach that path.
}
