# Installing Kio from source

This guide builds the `kio` binary and the VS Code extension from a source checkout, then wires the editor up to the binary so diagnostics, hover, and format-on-save work. You need Rust for the binary, plus Node.js and the Tree-sitter CLI for the extension. Provide the [native prerequisites](../../INSTALL.md#native-prerequisites), then install only these pinned tools from the repository root. On Windows, use Git Bash; Rust setup is supported and Node.js setup is best-effort:

```sh
sh ci/install-target-language-toolchains.sh rust js
sh ci/impl-toolchain.sh toolchain-general-install rust,js -- tree-sitter
```

The installer bootstraps its verified mise automatically. Rust and Node.js live
in the project toolchain store; Tree-sitter is a general mise helper. The commands
below select those tools explicitly, without account activation or profile edits.

The extension build runs `tree-sitter build --wasm`, which also needs the WASI SDK. Tree-sitter downloads it on first use unless it is already cached or supplied through `TREE_SITTER_WASI_SDK_PATH`, so allow network access for that first build. See [Tree-sitter's Wasm build instructions](https://tree-sitter.github.io/tree-sitter/cli/build.html#-w--wasm). Installing the extension requires VS Code's `code` command on your `PATH`.

## Building the `kio` binary

The `kio` binary is produced by the Rust crate in `kio-rs/`. From the repository
root, build it in the selected Rust environment:

```sh
sh ci/impl-toolchain.sh toolchain-exec rust -- cargo build --manifest-path kio-rs/Cargo.toml --release --bin kio
```

The compiled binary lands at `kio-rs/target/release/kio`.

The default Cargo features — `surface`, `prime`, `lsp`, `repl`, `parallel`, and `cli` — include `lsp`, which provides the `kio lsp` subcommand the editor spawns to serve diagnostics and other language features. A `cargo build --release --bin kio` build gives you everything the extension needs. If you build with `--no-default-features` for a trimmed binary, re-add `lsp` (for example `--no-default-features --features surface,cli,lsp`) or the editor will have nothing to talk to.

The user-facing surface of the language server is documented in [`specs/cli.md` § `kio lsp`](../../specs/cli.md#kio-lsp).

## Installing the VS Code extension from source

From the repository root:

```sh
sh ci/impl-toolchain.sh toolchain-exec js --helpers tree-sitter -- npm --prefix tools/vscode-kio install
sh ci/impl-toolchain.sh toolchain-exec js --helpers tree-sitter -- npm --prefix tools/vscode-kio run build
sh ci/impl-toolchain.sh toolchain-exec js --helpers tree-sitter -- npm --prefix tools/vscode-kio run package
code --install-extension tools/vscode-kio/kio.vsix
```

The extension always provides syntax highlighting, and adds diagnostics plus the rest of the language-server features once it finds a `kio` binary to spawn. For how the highlighting layers and the language-server client fit together, see [`tools/vscode-kio/README.md`](../../tools/vscode-kio/README.md).

## Pointing the extension at your binary

The extension looks for the `kio` binary in three places, in this order:

1. The `kio.server.path` user setting.
2. The `KIO_BIN` environment variable.
3. `kio` on your `PATH`.

The first nonempty explicit selection wins; otherwise the extension searches `PATH`. If you built with `cargo build --release --bin kio` and haven't put the binary on your `PATH`, set `kio.server.path` to its absolute location in your VS Code settings:

```jsonc
"kio.server.path": "/absolute/path/to/kio-rs/target/release/kio"
```

Changing `kio.server.path` restarts the language server with the selected binary.
In a remote workspace or Codespace, the path names a binary in that environment.
An invalid explicit setting or `KIO_BIN` prevents fallback to another compiler.

The Kio language status shows whether the server is starting, connected, or
unavailable. **Kio: Restart Language Server** retries with the current settings;
**Kio: Show Language Server Output** opens startup and failure details. After
changing the extension host's environment, reload the window so it inherits
those changes. Browser-only extension hosts provide highlighting and display
that mode without an installation warning; a browser connected to a remote
Node workspace can run the language server there.

An unavailable compiler or a server that cannot start produces an actionable
notification. Unexpected exits receive a bounded automatic retry; repeated
failures leave an unavailable status and a manual retry action. Dismissing a
notification keeps the status and Output commands available. Ordinary source
diagnostics and differing compiler version numbers do not produce availability
warnings. The `kio.trace.server` setting enables additional protocol logging;
its default is `off`, and verbose traces can include source text.

## Format on save

Open the Command Palette and choose **Preferences: Open User Settings (JSON)** to edit `settings.json`. For project-specific settings, choose **Preferences: Open Workspace Settings (JSON)** instead.

To format Kio files automatically when saving, merge the following into `settings.json`. Keep your other settings; if `"[kio]"` already exists, add the two entries to that section.

```jsonc
{
  "[kio]": {
    "editor.defaultFormatter": "kio-lang.kio",
    "editor.formatOnSave": true
  }
}
```

Formatting is served by `kio lsp`, so this requires a resolvable `kio` binary (set via any of the three discovery rules above). In highlighting-only mode — when no binary is found — there is no formatter to invoke. The formatter mirrors the standalone [`kio fmt`](../../specs/cli.md#kio-fmt) command.

## See also

- [`INSTALL.md`](../../INSTALL.md) — the pinned toolchain and local setup paths.
- [`CONTRIBUTING.md`](../../CONTRIBUTING.md) — the contribution workflow.
- [`tools/vscode-kio/README.md`](../../tools/vscode-kio/README.md) — the extension's feature layers, build steps, and language-server configuration.
- [Shell completions for `kio`](shell-completions.md) — generating and installing a completion script for your shell.
- [`specs/cli.md`](../../specs/cli.md) — the CLI reference, including `kio lsp` and `kio fmt`.
