# Installing Kio from source

This guide builds the `kio` binary and the VS Code extension from a source checkout, then wires the editor up to the binary so diagnostics, hover, and format-on-save work. You need a Rust toolchain for the binary, plus Node.js and the Tree-sitter CLI for the extension. After installing and activating mise as described in [`INSTALL.md`](../../INSTALL.md), install their pinned versions from the repository root:

```sh
mise trust mise.toml
mise install --locked rust node tree-sitter
```

The extension build runs `tree-sitter build --wasm`, which also needs the WASI SDK. Tree-sitter downloads it on first use unless it is already cached or supplied through `TREE_SITTER_WASI_SDK_PATH`, so allow network access for that first build. See [Tree-sitter's Wasm build instructions](https://tree-sitter.github.io/tree-sitter/cli/build.html#-w--wasm). Installing the extension requires VS Code's `code` command on your `PATH`.

## Building the `kio` binary

The `kio` binary is produced by the Rust crate in `kio-rs/`. Build it from there:

```sh
cd kio-rs
cargo build --release --bin kio
```

The compiled binary lands at `kio-rs/target/release/kio`.

The default Cargo features — `surface`, `prime`, `lsp`, `repl`, `parallel`, and `cli` — include `lsp`, which provides the `kio lsp` subcommand the editor spawns to serve diagnostics and other language features. A `cargo build --release --bin kio` build gives you everything the extension needs. If you build with `--no-default-features` for a trimmed binary, re-add `lsp` (for example `--no-default-features --features surface,cli,lsp`) or the editor will have nothing to talk to.

The user-facing surface of the language server is documented in [`specs/cli.md` § `kio lsp`](../../specs/cli.md#kio-lsp).

## Installing the VS Code extension from source

From the repository root:

```sh
cd tools/vscode-kio
npm install
npm run build
npm run package        # -> kio.vsix
code --install-extension kio.vsix
```

The extension always provides syntax highlighting, and adds diagnostics plus the rest of the language-server features once it finds a `kio` binary to spawn. For how the highlighting layers and the language-server client fit together, see [`tools/vscode-kio/README.md`](../../tools/vscode-kio/README.md).

## Pointing the extension at your binary

The extension looks for the `kio` binary in three places, in this order:

1. The `kio.server.path` user setting.
2. The `KIO_BIN` environment variable.
3. `kio` on your `PATH`.

The first one that resolves to an existing executable wins. If you built with `cargo build --release --bin kio` and haven't put the binary on your `PATH`, set `kio.server.path` to its absolute location in your VS Code settings:

```jsonc
"kio.server.path": "/absolute/path/to/kio-rs/target/release/kio"
```

Changing `kio.server.path` requires a window reload (Developer: Reload Window) before the extension picks up the new binary.

## Format on save

To format `.kio` files automatically when you save, add this to your user settings:

```jsonc
"[kio]": {
  "editor.defaultFormatter": "kio-lang.kio",
  "editor.formatOnSave": true
}
```

Formatting is served by `kio lsp`, so this requires a resolvable `kio` binary (set via any of the three discovery rules above). In highlighting-only mode — when no binary is found — there is no formatter to invoke. The formatter mirrors the standalone [`kio fmt`](../../specs/cli.md#kio-fmt) command.

## See also

- [`INSTALL.md`](../../INSTALL.md) — the pinned toolchain and local setup paths.
- [`CONTRIBUTING.md`](../../CONTRIBUTING.md) — the contribution workflow.
- [`tools/vscode-kio/README.md`](../../tools/vscode-kio/README.md) — the extension's feature layers, build steps, and language-server configuration.
- [Shell completions for `kio`](shell-completions.md) — generating and installing a completion script for your shell.
- [`specs/cli.md`](../../specs/cli.md) — the CLI reference, including `kio lsp` and `kio fmt`.
