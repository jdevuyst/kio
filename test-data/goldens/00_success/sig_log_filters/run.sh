#!/bin/sh
# `kio sig log` display filters: `--breaking` shows only versions with a
# breaking section (and only their breaking entries); `--since N` shows
# only versions after N; the two combine. This case builds a
# three-version changelog with mixed breaking / nonbreaking content and
# asserts each filter scopes correctly.
#
# The flow *mutates* the package, so it runs in a private scratch copy of
# `workdir` rather than in `workdir` itself.
#
# Changelog messages are kept generic ("gen N") so the item-name
# assertions match the declarations themselves, not a message mention.
#
# `kio sig` is surface-only (kio-prime rejects it), so the case carries
# IS_KIO_PRIME plus SKIP_KIO_PRIME_RUN.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

# v(1): nonbreaking export add of `serve`.
"$KIO_BIN" sig stage >/dev/null 2>&1 || exit 1
"$KIO_BIN" sig commit -m "gen 1" >/dev/null 2>&1 || exit 1

# v(2): breaking removal of `serve` + nonbreaking add of `widen`.
printf 'module api;\n\npub fn widen() -> . { () }\n' > api.kio
"$KIO_BIN" sig stage --force >/dev/null 2>&1 || exit 1
"$KIO_BIN" sig commit -m "gen 2" >/dev/null 2>&1 || exit 1

# v(3): nonbreaking add of `extend`.
printf 'module api;\n\npub fn widen() -> . { () }\n\npub fn extend() -> . { () }\n' > api.kio
"$KIO_BIN" sig stage >/dev/null 2>&1 || exit 1
"$KIO_BIN" sig commit -m "gen 3" >/dev/null 2>&1 || exit 1

# Assert a substring appears (or, with log_lacks, does NOT appear) in the
# output of `kio sig log <args>`.
log_has() {
  want=$1
  shift
  "$KIO_BIN" sig log "$@" | grep -q "$want" || {
    # shellcheck disable=SC2016
    printf '`sig log %s` should contain %s\n' "$*" "$want" >&2
    "$KIO_BIN" sig log "$@" >&2
    exit 1
  }
}
log_lacks() {
  want=$1
  shift
  if "$KIO_BIN" sig log "$@" | grep -q "$want"; then
    # shellcheck disable=SC2016
    printf '`sig log %s` should NOT contain %s\n' "$*" "$want" >&2
    "$KIO_BIN" sig log "$@" >&2
    exit 1
  fi
}

# `--breaking` shows only v(2)'s breaking removal of `serve`, and drops
# every nonbreaking entry (`widen`, `extend`) and the non-breaking
# v(1)/v(3) blocks.
log_has 'serve' --breaking
log_lacks 'widen' --breaking
log_lacks 'extend' --breaking
log_lacks 'gen 1' --breaking
log_lacks 'gen 3' --breaking
log_has 'gen 2' --breaking

# `--breaking --since 1` is still just v(2) (v(2) > 1 and is breaking).
log_has 'serve' --breaking --since 1
log_lacks 'extend' --breaking --since 1

# `--since 2` scopes to versions after 2 — only v(3) (the `extend` add).
log_has 'extend' --since 2
log_lacks 'serve' --since 2
log_lacks 'gen 2' --since 2

# `--breaking --since 2` excludes the only breaking version (v(2) is not
# > 2), so no version block survives — `serve` does not appear.
log_lacks 'serve' --breaking --since 2

# A bare `kio sig log` (no filter) shows everything.
log_has 'serve'
log_has 'widen'
log_has 'extend'
