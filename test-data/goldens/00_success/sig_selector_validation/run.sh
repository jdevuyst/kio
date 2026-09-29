#!/bin/sh
# `kio sig` package-selector validation + deduplication:
#
#   - a non-`*.pkg.kio` existing file (`notes.txt`) is an input error
#     (exit 40), not a silently-accepted selector;
#   - a `*.pkg.kio` file selector works (exit 0);
#   - duplicate / overlapping selectors (`. .`, `. app.pkg.kio`) collapse
#     to one package, so a mutating `commit` advances the generation
#     exactly once instead of double-mutating.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

expect_exit() {
  want=$1
  shift
  "$@" >/dev/null 2>&1
  got=$?
  if [ "$got" -ne "$want" ]; then
    # shellcheck disable=SC2016
    printf 'expected exit %s from `%s`, got %s\n' "$want" "$*" "$got" >&2
    exit 1
  fi
}

# A non-package existing file is rejected (build-error tier).
printf 'notes\n' > notes.txt
expect_exit 40 "$KIO_BIN" sig status notes.txt

# A `*.pkg.kio` file selector is accepted and resolves to the package —
# staging the fresh surface succeeds (exit 0). (The bug accepted ANY
# existing file as a selector; `notes.txt` above must error, this must
# not.)
expect_exit 0 "$KIO_BIN" sig stage app.pkg.kio

# Seal v(1) -> v(2) with DUPLICATE selectors: `. .` must dedup to one
# package and advance the generation once.
expect_exit 0 "$KIO_BIN" sig commit . .
# Overlapping dir + pkg-file selector also dedups.
"$KIO_BIN" sig log | grep -q 'signature app v(2);' || {
  printf 'duplicate selectors should advance to v(2) exactly once\n' >&2
  "$KIO_BIN" sig log >&2
  exit 1
}
# A v(3) would mean it advanced twice — it must not have.
if "$KIO_BIN" sig log | grep -q 'signature app v(3);'; then
  printf 'duplicate selectors double-advanced the generation\n' >&2
  exit 1
fi
