# Kio language support

[Kio](https://jdevuyst.github.io/kio/) is an ultra-portable, embeddable programming language that compiles to host languages — write a package once and run it in any of them. This extension adds Kio support to VS Code and compatible editors.

Language-server features also work in browser-based remote workspaces, such as GitHub Codespaces.

## Features

- **Syntax highlighting** out of the box — a fast TextMate baseline, refined by a tree-sitter overlay for parser-accurate tokens.
- **Inline diagnostics** — precise type, parse, and elaboration errors with source context, right in your editor, from the `kio` language server.
- **Quick fixes and refactoring** — built-in code actions from the Kio language server.

## Requirements

Diagnostics, quick fixes, refactoring and formatting require the `kio` binary.

On macOS / Linux:

```sh
curl -fsSL https://jdevuyst.github.io/kio/install.sh | sh
```

On Windows (PowerShell):

```powershell
irm https://jdevuyst.github.io/kio/install.ps1 | iex
```

The extension finds it via the `kio.server.path` setting, then `KIO_BIN`, then your `PATH`. Without it, the extension runs in highlighting-only mode.

## Settings

Open the Command Palette and choose **Preferences: Open User Settings (JSON)** to edit `settings.json`. For project-specific settings, choose **Preferences: Open Workspace Settings (JSON)** instead.

The extension provides these settings:

- **`kio.server.path`** — path to the `kio` binary.
- **`kio.trace.server`** — LSP trace verbosity (`off` / `messages` / `verbose`).

### Format on save

To format Kio files automatically when saving, merge the following into `settings.json`. Keep your other settings; if `"[kio]"` already exists, add the two entries to that section.

```jsonc
{
  "[kio]": {
    "editor.defaultFormatter": "kio-lang.kio",
    "editor.formatOnSave": true
  }
}
```

## License

Dual-licensed under MIT or Apache-2.0.
