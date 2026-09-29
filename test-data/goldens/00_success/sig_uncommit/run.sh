#!/bin/sh
# `kio sig uncommit` pops the most-recently-sealed version block back
# into the open draft. It is `--force`-gated (it rewrites a sealed
# contract). This case asserts:
#   1. `uncommit` on a changelog with no sealed version errors;
#   2. after sealing v(1), `uncommit` WITHOUT `--force` is refused and
#      leaves the file unchanged;
#   3. `uncommit --force` pops the sealed block — the `v(2)` header steps
#      back to `v(1)` (the popped block is no longer sealed) and its
#      changes return as the open draft, so `kio sig status` reflects
#      them (clean, since the draft already records the live surface).
#
# The flow *mutates* the package (writes `app.sig.kio`), so it runs in a
# private scratch copy of `workdir` rather than in `workdir` itself.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME plus SKIP_KIO_PRIME_RUN.
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

# (3) Uncommit before anything is sealed: the changelog is still on its
# first (unsealed) draft, so there is nothing to pop -> error (exit 40,
# build-error tier). The package has no `*.sig.kio` yet, which is also
# the no-sealed-version case.
expect_exit 40 "$KIO_BIN" sig uncommit --force

# Seal v(1) -> open v(2).
expect_exit 0 "$KIO_BIN" sig stage
expect_exit 0 "$KIO_BIN" sig commit
# The header now names the open draft v(2); v(1) is sealed.
grep -q '^signature app v(2);$' app.sig.kio || {
  printf 'after commit the header should be v(2); file:\n' >&2
  cat app.sig.kio >&2
  exit 1
}

# (2) Uncommit without `--force` is refused and writes nothing.
cp app.sig.kio app.sig.kio.sealed
expect_exit 40 "$KIO_BIN" sig uncommit
if ! cmp -s app.sig.kio app.sig.kio.sealed; then
  # shellcheck disable=SC2016
  printf 'a refused `uncommit` must not modify app.sig.kio\n' >&2
  exit 1
fi

# (1) `uncommit --force` pops the sealed v(1) back into the draft: the
# header steps back to v(1) and the v(1) block's changes return as the
# open draft.
expect_exit 0 "$KIO_BIN" sig uncommit --force
grep -q '^signature app v(1);$' app.sig.kio || {
  # shellcheck disable=SC2016
  printf 'after `uncommit --force` the header should be v(1); file:\n' >&2
  cat app.sig.kio >&2
  exit 1
}
# The popped block (the export add of `serve`) is back as the open draft.
grep -q 'serve' app.sig.kio || {
  # shellcheck disable=SC2016
  printf 'the popped change `serve` should return as the open draft\n' >&2
  cat app.sig.kio >&2
  exit 1
}

# `kio sig status` reflects the returned draft: the draft records exactly
# the live surface against the (now-empty) sealed baseline, so it is
# clean (exit 0). Re-staging is a no-op that leaves the file stable.
expect_exit 0 "$KIO_BIN" sig status
