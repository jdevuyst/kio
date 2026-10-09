# Developing the Kio VS Code extension

Highlighting is a TextMate floor plus a tree-sitter overlay; diagnostics come from the `kio lsp` language server. This covers the architecture and the build — for using the extension, see [README.md](README.md).

## Architecture

Three feature layers, layered for redundancy and progressive enhancement:

1. **TextMate grammar** — the load-fast floor, wired through the manifest's `contributes.grammars` and painted before the extension's activation code runs. Bundled from [`../textmate-kio/kio.tmLanguage.json`](../textmate-kio/kio.tmLanguage.json) at build time.
2. **Tree-sitter semantic-tokens overlay** — refines the TextMate baseline with parser-context distinctions once `web-tree-sitter` loads the WASM. Each token tree-sitter classifies overrides the TextMate scope at the same range. The WASM is built at extension-build time from [`../tree-sitter-kio/`](../tree-sitter-kio/)'s committed `src/parser.c`.
3. **Language Server Protocol client** (local or remote Node extension hosts) — spawns `kio lsp` as a child process and surfaces its diagnostics. The server is the Rust pipeline ([`specs/cli.md` § `kio lsp`](../../specs/cli.md)); the extension provides the editor-side wiring via [`vscode-languageclient`](https://www.npmjs.com/package/vscode-languageclient). Additional LSP capabilities light up automatically as the server advertises them. The extension prefers the workspace host so remote environments, including browser-based Codespaces, run the server beside the workspace. Browser-only hosts retain highlighting without starting a server.

The client lifecycle owns language status, executable probing, restart and
Output commands, configuration changes, and shutdown. Automatic recovery reuses
one language-client error/restart budget per client. Failure tests exercise the
bundled extension against subprocess fixtures and verify recovered diagnostics,
notification deduplication, startup cancellation, and browser/remote routing.

For a focused editor test, set `KIO_DEBUG_VSCODE_TEST_FILTER` to a Mocha title
pattern when running the compiled `test/out/runTest.js`; omit it for the full
suite. `availability and recovery` selects lifecycle failure injection.

Cross-tokenizer agreement with `kio debug tokens` (the reference) is verified at build time by [`ci/checks/orchestrators/highlight-agreement.sh`](../../ci/checks/orchestrators/highlight-agreement.sh).

## Build

```sh
cd tools/vscode-kio
npm ci
npm run package   # -> kio.vsix
```

Run `npm ci` again after moving a checkout between operating systems or
changing `package-lock.json`. It replaces `node_modules` with the locked
dependencies for the current platform, including native tools such as esbuild.
The release packaging workflows use the same clean install.

`npm run package` runs the build and license preparation before creating the
VSIX. Run `npm run build` alone when you want compiled extension files without
packaging. The build steps:

- `build:textmate` — copies the TextMate JSON from `../textmate-kio/` into `syntaxes/`. The source-of-truth file lives outside the extension dir so the same grammar can be consumed by other surfaces; the copy keeps the extension self-contained for VS Code's loader.
- `build:wasm` — builds the Kio tree-sitter parser as WASM via `tree-sitter build --wasm` (output `parsers/tree-sitter-kio.wasm`). Requires `tree-sitter-cli`; on first run it downloads `wasi-sdk` (~110 MB) to `~/.cache/tree-sitter/`.
- `build:js` — bundles `src/extension.ts` to `dist/extension.js` via esbuild (Node platform, CJS, `vscode` external). This shared desktop/browser bundle includes `src/lsp.ts`, whose body and Node dependencies are evaluated only in a Node extension host. The runtime tests execute the browser entry without Node globals and reject Node module loads while exercising the bundled WASM highlighting.
- `build:wts-wasm` — copies `web-tree-sitter.wasm` from `node_modules/web-tree-sitter/` into `dist/`.

The published `.vsix` is ~1 MB unzipped; on-disk size is dominated by `vscode-languageclient` (~900 KB) and the two WASM blobs (Kio's parser ~12 KB, web-tree-sitter's runtime ~190 KB).

## Install a local build

Before installing or uploading a VSIX, inspect the contents of the actual
archive with `unzip -l kio.vsix` or, if Java is available, `jar tf kio.vsix`.
Check that it contains `extension/dist/extension.js`, the grammar and WASM
files, `extension/package.json`, and `extension/LICENSE.txt`, with no local
editor or agent settings. `npx vsce ls` previews the files selected for
packaging; inspect the archive as well because that is the file being installed
or published.

```sh
code --install-extension kio.vsix
```

Or VS Code → Extensions → "Install from VSIX".

## Scope

**In:** highlighting (TextMate floor + tree-sitter overlay) plus an LSP client that consumes `kio lsp` in local and remote Node extension hosts. Highlighting also runs in browser-only hosts, which have no child-process API. The editor's UI kind does not determine the extension host's runtime.

**Out:** the language server itself — that's `kio lsp` in the Rust pipeline.
