# Shell completions for `kio`

`kio completions <shell>` prints a completion script for your shell to standard output. Once the script is installed, your shell can complete `kio`'s subcommand names — `check`, `build`, `fmt`, `doc`, `cache`, and the rest — as you type them, along with the sub-subcommand names under `kio doc`, `kio cache`, `kio dep`, and `kio sig`, the shell names `kio completions` itself takes (`bash` / `zsh` / `fish`), and each subcommand's flags.

Three shells are supported: `bash`, `zsh`, and `fish`. Pick the one you use and follow the matching snippet below.

## What gets completed

The scripts complete the **fixed parts of the command line**: the subcommand words, the sub-subcommand words (`kio doc check`, `kio cache path`, …), and the flags each subcommand accepts. Where a subcommand takes a positional argument — a path to a source file for `kio fmt`, a target id for `kio build`, a module selector for `kio repl` — the script falls back to your shell's ordinary filename completion. The scripts do not read your package to suggest module names or target ids; what they complete is the shape of the `kio` command line itself, not the contents of your project.

The script reflects the `kio` you generated it with. If your `kio` was built without the optional `lsp` or `repl` subcommands, the script won't offer them — it matches exactly what `kio --help` lists. After you upgrade `kio` to a version with new subcommands, regenerate the script so the new commands complete too.

## bash

Generate the script and save it where your bash-completion setup loads per-command scripts. On most systems that is `/etc/bash_completion.d/` (system-wide) or `~/.local/share/bash-completion/completions/` (per user):

```sh
kio completions bash > ~/.local/share/bash-completion/completions/kio
```

Open a new shell, or source the file in your current one:

```sh
source ~/.local/share/bash-completion/completions/kio
```

This relies on the `bash-completion` package being installed and loaded from your `~/.bashrc` (it is by default on most distributions). To try the script without installing it, you can also source it directly:

```sh
source <(kio completions bash)
```

## zsh

zsh loads completion functions from the directories on its `fpath`. Save the script as a file named `_kio` on one of them. A common setup is a personal completions directory:

```sh
mkdir -p ~/.zsh/completions
kio completions zsh > ~/.zsh/completions/_kio
```

Then make sure that directory is on `fpath` *before* `compinit` runs, by adding these lines to your `~/.zshrc`:

```sh
fpath=(~/.zsh/completions $fpath)
autoload -Uz compinit && compinit
```

Open a new shell to pick up the change. If completions don't appear, clear the completion cache with `rm -f ~/.zcompdump*` and start a fresh shell.

## fish

fish autoloads completion scripts from `~/.config/fish/completions/`. Save the script there as `kio.fish`:

```sh
kio completions fish > ~/.config/fish/completions/kio.fish
```

fish picks it up in any new shell — no further configuration needed.

## See also

- [Getting started with the Kio tooling](../tutorials/tooling.md) — the core `kio` workflow the completions support.
- The CLI reference for `kio completions` lives in [`specs/cli.md`](../../specs/cli.md).
