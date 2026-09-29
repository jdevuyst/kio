#!/bin/sh
# `kio --version` (and its `-V` alias) print a single version line to
# stdout and exit 0, per specs/cli.md § Global options.
#
# The version *number* is deliberately not pinned here: that would make
# this case an untracked version mirror, and version-check.sh already owns
# the repo-version agreement. This case pins the line's *shape* and the
# `-V` / `--version` equivalence instead.
#
# Success emits nothing on stdout/stderr — the assertions are the
# contract; expected.stdout / expected.stderr are empty.
set -u

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

long=$("$KIO_BIN" --version) || fail "kio --version: non-zero exit"
short=$("$KIO_BIN" -V) || fail "kio -V: non-zero exit"

[ "$long" = "$short" ] \
  || fail "kio -V and --version disagree: '$short' vs '$long'"

# Shape: `kio <major>.<minor>.<patch>`, optionally followed by a
# `-dev (<commit>)` provenance marker for a build ahead of its release tag.
printf '%s\n' "$long" | grep -Eq '^kio [0-9]+\.[0-9]+\.[0-9]+' \
  || fail "kio --version: line is not 'kio <version>…' (got: '$long')"

# The flag is advertised under the top-level help's Options block.
"$KIO_BIN" --help | grep -q -- '--version' \
  || fail "kio --help: '--version' missing from Options"
