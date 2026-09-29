#!/bin/sh
# `kio completions bash` prints a bash completion script to stdout
# and exits 0. The script's exact text is implementation-specific
# (same convention `specs/exit-codes.md` applies to tool output), so
# the case pins behavior, not bytes: a non-empty script, the
# load-bearing `complete -F _kio kio` registration line, and a
# `compgen -W` offer for every subcommand the bare `kio` usage
# advertises (so a new subcommand that forgets to extend the
# completion grammar is caught here). When a `bash` is on PATH, the
# script is additionally syntax-checked with `bash -n`.
#
# Success emits nothing on stdout/stderr — the assertions are the
# contract; expected.stdout / expected.stderr are empty.
set -u

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

script=$("$KIO_BIN" completions bash) || fail "kio completions bash: non-zero exit"
[ -n "$script" ] || fail "kio completions bash: empty stdout"

printf '%s\n' "$script" | grep -q 'complete -F _kio kio' \
  || fail "kio completions bash: missing 'complete -F _kio kio' registration"

# Every subcommand the bare `kio` usage names must be completable.
# Derive the set from `kio --help` so the assertion tracks the
# binary's actual surface (the feature-gated `lsp` / `repl` lines
# appear only when compiled in).
"$KIO_BIN" --help | sed -n '/^Subcommands:/,/^Options:/p' \
  | grep -oE '^  [a-z][a-z-]*' | tr -d ' ' | while IFS= read -r cmd; do
  [ -n "$cmd" ] || continue
  printf '%s\n' "$script" | grep -q "$cmd" \
    || fail "kio completions bash: script does not complete subcommand '$cmd'"
done

# Syntax-check with bash when available; skip cleanly otherwise.
if command -v bash >/dev/null 2>&1; then
  printf '%s\n' "$script" > comp.bash
  bash -n comp.bash || fail "kio completions bash: generated script has a bash syntax error"
  rm -f comp.bash
fi
