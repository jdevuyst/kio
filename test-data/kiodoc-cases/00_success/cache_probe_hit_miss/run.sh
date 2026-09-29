#!/bin/sh
# Per-snippet doc cache: explicit hit/miss probe, then a snippet-body
# bust. `kio doc` keys each markdown `kio` snippet by its own content;
# a second run with no change must HIT every snippet, and editing one
# snippet's body must MISS only that snippet while the other still HITs.
#
# The neighbouring `cache_cold_warm` case asserts cold/warm byte
# determinism with the probe off; this case turns the
# KIO_DEBUG_DOC_CACHE probe on and pins the explicit hit/miss lines plus
# the per-snippet edit bust:
#   1. cold run populates the cache.
#   2. rerun with no change — both snippets HIT.
#   3. edit the `alpha` snippet body — `alpha` MISSes, `beta` HITs.
#
# Each `kiodoc-cache:` line carries the snippet's 64-hex key, not a
# name, so the assertion is by hit/miss count after normalizing the key
# and sorting.
#
# The case mutates `input.md`, so it runs in a private scratch copy of
# the package (the harness may run it on several impls against one
# shared case dir; in-place mutation would let them stomp each other).
set -eu

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

cp pkg.pkg.kio input.md "$scratch/"
cd "$scratch" || exit

probe() {
  grep -E '^kiodoc-cache: (hit|miss) ' "$1" | sed -E 's/[0-9a-f]{64}/<key>/' | sort
}

KIO_DEBUG_DOC_CACHE=1 "$KIO_BIN" doc check >/dev/null 2>cold.err

KIO_DEBUG_DOC_CACHE=1 "$KIO_BIN" doc check >/dev/null 2>warm.err

perl -0pi -e 's/pub fn alpha\(\) -> T \{ "alpha" \}/pub fn alpha() -> T { "ALPHA" }/' input.md
KIO_DEBUG_DOC_CACHE=1 "$KIO_BIN" doc check >/dev/null 2>edit.err

printf 'warm cache log:\n'
probe warm.err
printf 'edit cache log:\n'
probe edit.err
