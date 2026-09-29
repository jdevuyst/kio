#!/bin/sh
# Enriched-IR cache per-module key isolation. The enriched cache keys
# each module's recovered + optimized IR by that module's own content
# (plus the package-wide optimizer table). Editing one module's body
# must bust only that module's entry: its siblings keep hitting.
#
# Build once cold (every module misses, no entry yet), edit module `b`'s
# body without touching `a` or `c`, then rebuild. The artifact cache
# busts on the whole-package input change and re-runs the enriched pass,
# which now HITs `a` and `c` (their keys are unchanged) and MISSes `b`
# (its content changed, so its key moved). If per-module keys were not
# isolated, all three would miss.
#
# Each `enriched-cache:` line carries the module's 64-hex key, not its
# name, so the assertion is by hit/miss count after normalizing the key
# and sorting: cold is three misses, the post-edit build is two hits and
# one miss. The non-deterministic `compute` timing line is filtered out.
#
# Build a single target (the impl's own, `$KIO_TARGET`) so the
# enriched-cache log reflects one build's activity. The enriched cache
# is backend-agnostic (computed once before per-backend lowering), so a
# multi-target `kio build` would have the second target HIT every module
# the first just MISSed, doubling and reordering the log lines.
#
# The case mutates a module, so it runs in a private scratch copy of
# `workdir` (the harness may run it on several impls against one shared
# `workdir`; in-place mutation would let them stomp each other).
set -eu

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

mkdir "$scratch/pkg"
cp -R workdir/. "$scratch/pkg/"
cd "$scratch/pkg" || exit

KIO_DEBUG_ENRICHED_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>cold.err

perl -0pi -e 's/pub fn fb\(\) -> \. \{ \(\) \}/pub fn fb() -> . { let _ = (); () }/' b.kio
KIO_DEBUG_ENRICHED_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>edit.err

printf 'cold cache log:\n'
grep -E '^enriched-cache: (hit|miss) ' cold.err | sed -E 's/[0-9a-f]{64}/<key>/' | sort
printf 'edit cache log:\n'
grep -E '^enriched-cache: (hit|miss) ' edit.err | sed -E 's/[0-9a-f]{64}/<key>/' | sort
