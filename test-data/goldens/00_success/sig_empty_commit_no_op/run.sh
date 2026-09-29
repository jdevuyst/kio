#!/bin/sh
# `kio sig commit` of an empty draft must be a no-op: an empty draft
# records nothing, so there is nothing to seal. Sealing it would mint a
# phantom version generation with no block (a non-contiguous changelog).
# The pinned bug advanced the generation anyway; the fix prints a
# nothing-to-seal message, exits 0, and writes no `*.sig.kio`.
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

# The package exposes no contract items, so the draft is empty. `commit`
# is a no-op success (nothing to seal) and writes no changelog.
expect_exit 0 "$KIO_BIN" sig commit
if [ -e app.sig.kio ]; then
  printf 'empty-draft commit must not write a changelog\n' >&2
  exit 1
fi
# Idempotent: a second commit is still a no-op.
expect_exit 0 "$KIO_BIN" sig commit
if [ -e app.sig.kio ]; then
  printf 'empty-draft commit must remain a no-op\n' >&2
  exit 1
fi
