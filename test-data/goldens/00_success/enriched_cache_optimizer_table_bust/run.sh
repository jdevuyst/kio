#!/bin/sh
# Enriched-IR cache busts on a module-relevant optimizer-table change. The
# enriched cache key folds in the consumer module's resolved newtype-member
# side table (`OptimizerCtx::newtype_members`), not just its own bytes. A
# renamed member therefore invalidates the declaring module and a consumer
# that imports the newtype, while an unrelated module remains a cache hit.
#
# Three steps make the dependency sharp:
#   1. cold build — every module misses.
#   2. edit module `a`'s ordinary fn body (not the newtype) — the
#      artifact cache busts, the enriched pass re-runs, and the siblings
#      `b`/`c` HIT (their per-module bytes and the optimizer table are
#      unchanged); only `a` misses.
#   3. rename the newtype member `mk_wrap` -> `make_wrap` in `a` — the
#      optimizer table changes for `a` and importing module `b`, so both
#      MISS; unrelated module `c` HITs. Step 2's two sibling hits versus
#      step 3's one hit and two misses covers both relevance and busting.
#
# Each `enriched-cache:` line carries a 64-hex key, not a module name, so
# the assertion is by hit/miss count after normalizing the key and
# sorting. The non-deterministic `compute` timing line is filtered out.
#
# Build a single target (the impl's own, `$KIO_TARGET`): the enriched
# cache is backend-agnostic (computed once before per-backend lowering), so
# a multi-target `kio build` would have the second target HIT every
# module the first MISSed, doubling and reordering the log lines.
#
# The case mutates modules, so it runs in a private scratch copy of
# `workdir` (the harness may run it on several impls against one shared
# `workdir`; in-place mutation would let them stomp each other).
set -eu

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

mkdir "$scratch/pkg"
cp -R workdir/. "$scratch/pkg/"
cd "$scratch/pkg" || exit

KIO_DEBUG_ENRICHED_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>cold.err

perl -0pi -e 's/pub fn wrap\(n: I32\) -> Wrap \{ Wrap\.mk_wrap\(n\) \}/pub fn wrap(n: I32) -> Wrap { let m = n; Wrap.mk_wrap(m) }/' a.kio
KIO_DEBUG_ENRICHED_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>body.err

perl -0pi -e 's/pub constructor mk_wrap;/pub constructor make_wrap;/; s/Wrap\.mk_wrap/Wrap.make_wrap/' a.kio
KIO_DEBUG_ENRICHED_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>rename.err

printf 'cold cache log:\n'
grep -E '^enriched-cache: (hit|miss) ' cold.err | sed -E 's/[0-9a-f]{64}/<key>/' | sort
printf 'body-edit cache log:\n'
grep -E '^enriched-cache: (hit|miss) ' body.err | sed -E 's/[0-9a-f]{64}/<key>/' | sort
printf 'member-rename cache log:\n'
grep -E '^enriched-cache: (hit|miss) ' rename.err | sed -E 's/[0-9a-f]{64}/<key>/' | sort
