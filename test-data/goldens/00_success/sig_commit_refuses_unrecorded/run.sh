#!/bin/sh
# `kio sig commit` must refuse to seal a draft that does not already
# record the live delta — the worktree is unreconciled. From a sealed
# baseline (v(1) records `api.serve`, header at v(2)), the live source
# adds a compatible export `api.extra` that was never staged. `commit`
# without a prior `stage` must:
#
#   (1) exit 40 (build-error tier) — NOT silently seal a surface that
#       differs from what the draft claims; and
#   (2) leave the `*.sig.kio` byte-for-byte unchanged (no sealed file
#       written, the changelog still at v(2) with the v(1) block only).
#
# The flow mutates (a successful commit would rewrite `app.sig.kio`), so
# it runs in a private scratch copy of `workdir`.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module source is Kio'-shaped) plus SKIP_KIO_PRIME_RUN
# to opt out of the kio-prime run.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio workdir/app.sig.kio "$scratch/" || exit 1
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

# Snapshot the sealed changelog before the refused commit.
before=$(cat app.sig.kio)

# `commit` with an unrecorded compatible add is a build-error refusal.
expect_exit 40 "$KIO_BIN" sig commit

# The refusal must not have written a sealed file: the changelog is
# byte-for-byte what it was (still v(2), v(1) block only — no v(3)).
after=$(cat app.sig.kio)
if [ "$before" != "$after" ]; then
  # shellcheck disable=SC2016
  printf 'a refused `kio sig commit` must not rewrite app.sig.kio\n' >&2
  printf -- '--- before ---\n%s\n--- after ---\n%s\n' "$before" "$after" >&2
  exit 1
fi

# Sanity: staging the compatible add first, then committing, succeeds —
# the refusal is specifically about the *unrecorded* delta.
expect_exit 0 "$KIO_BIN" sig stage
expect_exit 0 "$KIO_BIN" sig commit
"$KIO_BIN" sig log | grep -q 'signature app v(3);' || {
  printf 'after staging then committing, the changelog should reach v(3)\n' >&2
  "$KIO_BIN" sig log >&2
  exit 1
}
