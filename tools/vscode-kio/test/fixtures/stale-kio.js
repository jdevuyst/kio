#!/usr/bin/env node
// Stand-in for a `kio` binary that cannot serve LSP.
//
// This is the shape of the bug this fixture exists to pin: a binary left
// in `kio-rs/target/` by some earlier build, old enough to predate the
// `lsp` subcommand (or built without the `lsp` cargo feature). It answers
// `--help` with a subcommand list that has no `lsp` in it, and answers
// `kio lsp` the way the real CLI does — a usage error on stderr, exit 2.
//
// The extension is supposed to notice that from `--help` and never spawn
// it. If it spawns it anyway, the language client retries the identical
// failure until its restart budget runs out. `MARKER_ENV` is how the test
// tells those apart: this script records the fact that it was asked to
// serve, and the test asserts the file was never written.

"use strict";

const fs = require("node:fs");

const MARKER_ENV = "VSCODE_KIO_STALE_MARKER";
const args = process.argv.slice(2);

if (args.includes("--help")) {
  process.stdout.write(
    [
      "Usage: kio <subcommand> [args]",
      "",
      "Subcommands:",
      "  check                       Typecheck the current package.",
      "  build [<target-id>...]      Transpile to one or more compilation targets.",
      "  fmt [<path>...]             Format Kio source files in place.",
      "",
    ].join("\n"),
  );
  process.exit(0);
}

if (args[0] === "lsp") {
  const marker = process.env[MARKER_ENV];
  if (marker !== undefined && marker !== "") {
    fs.writeFileSync(marker, "the extension spawned a server that cannot serve\n");
  }
  process.stderr.write("error: unknown subcommand: lsp\n");
  process.exit(2);
}

process.exit(0);
