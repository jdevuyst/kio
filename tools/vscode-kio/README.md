# Kio language support

[Kio](https://jdevuyst.github.io/kio/) is an ultra-portable, embeddable programming language that compiles to host languages — write a package once and run it in any of them. This extension adds Kio support to VS Code and compatible editors.

## Features

- **Syntax highlighting** out of the box — a fast TextMate baseline, refined by a tree-sitter overlay for parser-accurate tokens.
- **Inline diagnostics** — precise type, parse, and elaboration errors with source context, right in your editor, from the `kio` language server.
- Highlighting works in desktop and web editors; diagnostics are desktop-only.

## Requirements

Diagnostics need the `kio` binary. On macOS / Linux:

```sh
curl -fsSL https://jdevuyst.github.io/kio/install.sh | sh
```

On Windows (PowerShell):

```powershell
irm https://jdevuyst.github.io/kio/install.ps1 | iex
```

The extension finds it via the `kio.server.path` setting, then `KIO_BIN`, then your `PATH`. Without it, the extension runs in highlighting-only mode.

## Settings

- **`kio.server.path`** — path to the `kio` binary.
- **`kio.trace.server`** — LSP trace verbosity (`off` / `messages` / `verbose`).

## Format on save

```jsonc
"[kio]": {
  "editor.defaultFormatter": "kio-lang.kio",
  "editor.formatOnSave": true
}
```

Formatting is served by `kio lsp`, so it needs a resolvable `kio` binary.

Dual-licensed under MIT or Apache-2.0.
