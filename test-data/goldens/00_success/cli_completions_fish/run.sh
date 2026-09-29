#!/bin/sh
# `kio completions fish` prints a fish completion script to stdout
# and exits 0. The script's exact text is implementation-specific
# (same convention `specs/exit-codes.md` applies to tool output), so
# the case pins behavior, not bytes: a non-empty script, the
# load-bearing `complete -c kio` directive, and a `__fish_use_sub-
# command` offer for every subcommand the bare `kio` usage
# advertises. When a `fish` is on PATH, the script is additionally
# syntax-checked with `fish --no-execute`.
#
# Success emits nothing on stdout/stderr — the assertions are the
# contract; expected.stdout / expected.stderr are empty.
set -u

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

script=$("$KIO_BIN" completions fish) || fail "kio completions fish: non-zero exit"
[ -n "$script" ] || fail "kio completions fish: empty stdout"

printf '%s\n' "$script" | grep -q '^complete -c kio' \
  || fail "kio completions fish: missing 'complete -c kio' directive"

"$KIO_BIN" --help | sed -n '/^Subcommands:/,/^Options:/p' \
  | grep -oE '^  [a-z][a-z-]*' | tr -d ' ' | while IFS= read -r cmd; do
  [ -n "$cmd" ] || continue
  printf '%s\n' "$script" \
    | grep -q "__fish_use_subcommand' -a '$cmd'" \
    || fail "kio completions fish: script does not offer subcommand '$cmd'"
done

if command -v fish >/dev/null 2>&1; then
  printf '%s\n' "$script" > kio.fish
  fish --no-execute kio.fish \
    || fail "kio completions fish: generated script has a fish syntax error"
  rm -f kio.fish
fi
