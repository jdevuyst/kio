#!/bin/sh
# Forced-break auto-retraction: a break force-recorded into the OPEN
# (unsealed) draft disappears once the source is reconciled back to the
# last sealed contract — the draft is recomputed from scratch every run
# as `diff(sealed, live)`, so a no-longer-present break is no longer
# recorded.
#
# Seal v(1) with `pub fn serve`. Drop `serve` and `stage --force` the
# break into the open v(2) draft (status 82). Then RESTORE the sealed
# shape (re-add `pub fn serve`) and re-stage:
#
#   (1) `kio sig status` returns to 0; and
#   (2) the open draft no longer carries a `v(2)` block at all (and so no
#       `breaking { }` section) — the forced break self-retracted.
#
# The flow mutates (rewrites `api.kio`, writes `app.sig.kio`), so it runs
# in a private scratch copy of `workdir`.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME (its module source is Kio'-shaped) plus SKIP_KIO_PRIME_RUN
# to opt out of the kio-prime run.
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

# Seal v(1) with `pub fn serve`.
expect_exit 0 "$KIO_BIN" sig stage
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# Break: drop `serve`; force-record it into the OPEN v(2) draft (do NOT
# seal). Status is 82 (recorded break, pending).
printf 'module api;\n\npub fn other() -> . { () }\n' > api.kio
expect_exit 0 "$KIO_BIN" sig stage --force
expect_exit 82 "$KIO_BIN" sig status
grep -Eq 'v\(2\) \{' app.sig.kio || {
  printf 'the forced break should have opened a v(2) draft block\n' >&2
  cat app.sig.kio >&2
  exit 1
}

# RESTORE the sealed v(1) shape: re-add `pub fn serve`, drop `other`. The
# break no longer exists vs the sealed contract.
printf 'module api;\n\npub fn serve() -> . { () }\n' > api.kio

# Re-stage reconciles the stale draft. The recompute (diff(v(1), live))
# is empty, so the forced break self-retracts.
expect_exit 0 "$KIO_BIN" sig stage

# Status returns to clean.
expect_exit 0 "$KIO_BIN" sig status

# The open draft no longer carries a `v(2)` block — and therefore no
# `breaking { }` section. (The header line `signature app v(2);` is not a
# block; `v(2) {` is.)
if grep -Eq 'v\(2\) \{' app.sig.kio; then
  printf 'the open draft must no longer carry a v(2) breaking block after retraction\n' >&2
  cat app.sig.kio >&2
  exit 1
fi
