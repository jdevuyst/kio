#!/bin/sh
# `--force` is honored only on `kio sig stage` (it records a break into
# the draft). On any other subcommand it must be a CLI usage error
# (exit 2), NOT a silently-honored forced record — the pinned bug let
# `--force` clobber the parsed subcommand, so `status --force` /
# `log --force` turned a read-only command into a mutating forced
# record. This case asserts the Usage rejection on each non-stage
# subcommand AND that no `*.sig.kio` is written by any of them.
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

# Each non-stage subcommand with `--force` is a usage error (exit 2).
# A bare `kio sig --force` (no subcommand) is the non-mutating status
# summary plus `--force`, which is also a usage error.
expect_exit 2 "$KIO_BIN" sig --force
expect_exit 2 "$KIO_BIN" sig status --force
expect_exit 2 "$KIO_BIN" sig log --force
expect_exit 2 "$KIO_BIN" sig commit --force
expect_exit 2 "$KIO_BIN" sig compact 1 --force

# None of the above may have written a changelog (they all rejected
# before any mutation).
if [ -e app.sig.kio ]; then
  # shellcheck disable=SC2016
  printf 'a rejected `--force` subcommand must not write app.sig.kio\n' >&2
  exit 1
fi

# Sanity: `kio sig stage --force` (the one place --force is valid) still
# records and writes.
expect_exit 0 "$KIO_BIN" sig stage --force
if [ ! -e app.sig.kio ]; then
  # shellcheck disable=SC2016
  printf '`kio sig stage --force` should record and write app.sig.kio\n' >&2
  exit 1
fi
