#!/bin/sh
# `kio completions zsh` prints a zsh completion script to stdout and
# exits 0. The script's exact text is implementation-specific (same
# convention `specs/exit-codes.md` applies to tool output), so the
# case pins behavior, not bytes: a non-empty script, the
# load-bearing `#compdef kio` autoload header, and an entry for every
# subcommand the bare `kio` usage advertises. When a `zsh` is on
# PATH, the script is additionally syntax-checked with `zsh -n`.
#
# Success emits nothing on stdout/stderr — the assertions are the
# contract; expected.stdout / expected.stderr are empty.
set -u

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

script=$("$KIO_BIN" completions zsh) || fail "kio completions zsh: non-zero exit"
[ -n "$script" ] || fail "kio completions zsh: empty stdout"

printf '%s\n' "$script" | head -n1 | grep -q '^#compdef kio$' \
  || fail "kio completions zsh: missing '#compdef kio' autoload header on line 1"

"$KIO_BIN" --help | sed -n '/^Subcommands:/,/^Options:/p' \
  | grep -oE '^  [a-z][a-z-]*' | tr -d ' ' | while IFS= read -r cmd; do
  [ -n "$cmd" ] || continue
  printf '%s\n' "$script" | grep -q "$cmd" \
    || fail "kio completions zsh: script does not complete subcommand '$cmd'"
done

if command -v zsh >/dev/null 2>&1; then
  printf '%s\n' "$script" > _kio
  zsh -n _kio || fail "kio completions zsh: generated script has a zsh syntax error"
  rm -f _kio
fi
