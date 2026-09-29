// `@vscode/test-electron` entry point.
//
// Downloads (or reuses a cached) VS Code, launches it pointed at
// the V.1 extension under development, and runs the Mocha suite
// inside the running editor. CI invokes this via
// `ci/checks/orchestrators/vscode-e2e.sh`; locally,
// `npm test` works under Xvfb on Linux.
//
// The extension resolves its language server exactly once, when it
// activates, and the only seam that lands before that is a
// `.vscode/settings.json` written into the launch workspace —
// `getConfiguration().update()` happens too late, since the extension
// has already activated and resolved by then. So one launch exercises
// exactly one server, and the three servers worth exercising get a
// launch each, selected by `VSCODE_KIO_E2E_MODE`:
//
//   mock  — `fixtures/mock-lsp.js`, a canned JSON-RPC responder. Pins
//           the editor-side wiring (a published diagnostic reaches
//           `vscode.languages.getDiagnostics`) deterministically, and
//           without a Rust build.
//   real  — the actual `kio` binary named by `$KIO_E2E_SERVER`, serving
//           `kio lsp` over stdio against a real Kio package. Pins what
//           the mock structurally cannot: that the extension spawns the
//           real server the way the real server expects.
//   stale — `fixtures/stale-kio.js`, a `kio` too old to have `kio lsp`.
//           Pins that the extension learns that from `--help` and
//           declines to spawn it.
//
// `ci/checks/orchestrators/vscode-e2e.sh` runs all three.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { runTests } from "@vscode/test-electron";

type Mode = "mock" | "real" | "stale";

function mode(): Mode {
  const raw = process.env.VSCODE_KIO_E2E_MODE ?? "mock";
  if (raw !== "mock" && raw !== "real" && raw !== "stale") {
    throw new Error(
      `VSCODE_KIO_E2E_MODE must be mock | real | stale (got ${raw})`,
    );
  }
  return raw;
}

// Fixtures the activation / highlighting suites open. They sit flat at
// the workspace root and are staged only for the mock launch: a real
// server resolves their `module` paths against the workspace root and
// rejects the mismatch as a parse error, which is not the diagnostic
// the real launch is there to assert.
const FLAT_FIXTURES = [
  "minimal.kio",
  "highlight.kio",
  "signature.sig.kio",
  "dependency.dep.kio",
  "dependency.lock.kio",
];

function serverFor(m: Mode, fixturesDir: string): string {
  switch (m) {
    case "mock":
      return path.join(fixturesDir, "mock-lsp.js");
    case "stale":
      return path.join(fixturesDir, "stale-kio.js");
    case "real": {
      const bin = process.env.KIO_E2E_SERVER;
      if (bin === undefined || bin === "") {
        throw new Error(
          "VSCODE_KIO_E2E_MODE=real needs KIO_E2E_SERVER pointing at a built `kio`",
        );
      }
      if (!fs.existsSync(bin)) {
        throw new Error(`KIO_E2E_SERVER does not exist: ${bin}`);
      }
      return bin;
    }
  }
}

async function main(): Promise<void> {
  // The extension's root is two levels up from this file:
  // `test/out/runTest.js` → `test/` → `tools/vscode-kio/`.
  const extensionDevelopmentPath = path.resolve(__dirname, "..", "..");
  // Mocha suite entry, compiled from `test/suite/index.ts` to
  // `test/out/suite/index.js`.
  const extensionTestsPath = path.resolve(__dirname, "suite", "index.js");
  const fixturesDir = path.resolve(__dirname, "..", "..", "test", "fixtures");

  const m = mode();
  const workspaceDir = fs.mkdtempSync(
    path.join(os.tmpdir(), "vscode-kio-e2e-"),
  );

  fs.mkdirSync(path.join(workspaceDir, ".vscode"), { recursive: true });
  fs.writeFileSync(
    path.join(workspaceDir, ".vscode", "settings.json"),
    JSON.stringify({ "kio.server.path": serverFor(m, fixturesDir) }, null, 2),
  );

  if (m === "mock") {
    for (const name of FLAT_FIXTURES) {
      fs.symlinkSync(
        path.join(fixturesDir, name),
        path.join(workspaceDir, name),
      );
    }
  } else {
    // A real Kio package, in a subdirectory so the server's package-root
    // walk scopes analysis to it. `main.kio` carries one deliberate type
    // error — `kio check` on this package exits 14.
    fs.cpSync(
      path.join(fixturesDir, "lsp-pkg"),
      path.join(workspaceDir, "lsp-pkg"),
      { recursive: true },
    );
  }

  // The stale launch asserts a negative — that a server which cannot
  // serve was never spawned. The fixture writes this file if it ever is,
  // which is the only thing that separates "the extension declined to
  // spawn it" from "the extension spawned it and the resulting crash
  // published no diagnostics".
  const staleMarker = path.join(workspaceDir, "stale-server-was-spawned");

  try {
    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [workspaceDir, "--disable-extensions"],
      // Passed to the suite so tests can resolve fixture paths against
      // the workspace without re-deriving them from cwd.
      extensionTestsEnv: {
        VSCODE_KIO_TEST_WORKSPACE: workspaceDir,
        VSCODE_KIO_E2E_MODE: m,
        VSCODE_KIO_STALE_MARKER: staleMarker,
      },
    });
  } catch (err) {
    console.error(`Failed to run tests (mode: ${m}):`, err);
    process.exit(1);
  } finally {
    fs.rmSync(workspaceDir, { recursive: true, force: true });
  }
}

void main();
