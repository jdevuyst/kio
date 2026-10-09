#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const controlPath = process.env.KIO_TEST_LSP_CONTROL;
const control = controlPath ? JSON.parse(fs.readFileSync(controlPath, "utf8")) : {};
if (process.argv.includes("--help")) {
  if (controlPath) fs.appendFileSync(`${controlPath}.probes`, `${process.pid}\n`);
  if (control.mode === "probe-timeout") {
    setInterval(() => {}, 1000);
  } else {
    process.stdout.write("Subcommands:\n  lsp  Language server\n");
    process.exit(control.mode === "probe-failure" ? 7 : 0);
  }
} else {
  fs.appendFileSync(`${controlPath}.starts`, `${process.pid}\n`);
  if (control.mode === "startup-exit") process.exit(9);
  let buffer = Buffer.alloc(0);
  process.stdin.on("end", () => { if (control.mode !== "transport-close-alive") process.exit(0); });
  process.stdin.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      const end = buffer.indexOf("\r\n\r\n");
      if (end < 0) return;
      const length = Number(/Content-Length:\s*(\d+)/i.exec(buffer.subarray(0, end).toString())[1]);
      if (buffer.length < end + 4 + length) return;
      const message = JSON.parse(buffer.subarray(end + 4, end + 4 + length).toString());
      buffer = buffer.subarray(end + 4 + length);
      handle(message);
    }
  });
}

function send(message) {
  const body = JSON.stringify({ jsonrpc: "2.0", ...message });
  process.stdout.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

function handle(message) {
  const first = fs.readFileSync(`${controlPath}.starts`, "utf8").trim().split("\n").length === 1;
  if (message.method === "initialize") {
    fs.appendFileSync(`${controlPath}.initializes`, `${process.pid}\n`);
    if (control.mode === "initialize-timeout" || (control.mode === "crash-then-hang" && !first)) return;
    if (control.mode === "initialize-error") {
      send({ id: message.id, error: { code: -32603, message: "injected initialization failure https://user:secret@example.invalid/" } });
    } else {
      send({ id: message.id, result: {
        capabilities: { textDocumentSync: { openClose: true, change: 1 }, hoverProvider: true },
        serverInfo: { name: "fault-lsp", version: control.version ?? "0.0.0" },
      } });
    }
  } else if (message.method === "initialized" && (control.mode === "crash" || (first && ["crash-once", "crash-then-hang"].includes(control.mode)))) {
    setTimeout(() => process.exit(9), 50);
  } else if (message.method === "initialized" && control.mode === "transport-error") {
    for (let index = 0; index < 5; index++) setTimeout(() => process.stdout.write("Content-Length: 1\r\n\r\n{"), 20 * index);
  } else if (message.method === "initialized" && control.mode === "transport-close-alive") {
    process.stdout.end();
    setInterval(() => {}, 1000);
  } else if (message.method === "textDocument/didOpen" || message.method === "textDocument/didChange") {
    const document = message.params.textDocument;
    send({ method: "textDocument/publishDiagnostics", params: {
      uri: document.uri,
      version: document.version,
      diagnostics: [{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 6 } },
        severity: 1, source: "availability-fixture", message: "analysis from the recovered server" }],
    } });
  } else if (message.method === "textDocument/hover") {
    send({ id: message.id, error: { code: ["request-error", "request-error-crash"].includes(control.mode) ? -32603 : -32803, message: "injected request failure" } });
    if (first && control.mode === "request-error-crash") setTimeout(() => process.exit(9), 100);
  } else if (message.method === "shutdown") {
    send({ id: message.id, result: null });
  } else if (message.method === "exit") {
    process.exit(0);
  }
}
