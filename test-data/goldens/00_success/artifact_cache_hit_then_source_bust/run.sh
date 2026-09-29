#!/bin/sh
# Artifact (emitted-crate) cache: warm HIT, then a source-class bust. The
# artifact cache keys the emitted target output by its full input
# fingerprint; a second build with no input change must HIT and serve the
# cached artifact, and a module-body edit must MISS and regenerate.
#
# The existing sig-cache golden only varies the `*.sig.kio` input class;
# this case pins the module-source input class:
#   1. cold build — miss (no entry), artifact written.
#   2. rebuild with no change — HIT.
#   3. edit the module body — MISS (the source fingerprint moved).
#
# The `artifact-cache:` line carries the target name and the 64-hex key,
# which is normalized.
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

# Build a single target — the impl's own (`$KIO_TARGET`) — so the cache
# probe stays deterministic. The artifact cache keys output per target;
# the source-class bust this case pins behaves identically for any one
# target, and building only one keeps the probe output a fixed line
# count regardless of how many targets the package declares (parallel
# per-target builds would otherwise interleave non-deterministically).
# The JS row is one routing witness for that shared fingerprint transition;
# it does not claim to exercise every backend emitter's cache restoration.
# The probe normalizes the 64-hex key and the target name so the
# expected output is impl-agnostic.
probe() {
  grep -E '^artifact-cache: (hit|miss) ' "$1" \
    | sed -E 's/[0-9a-f]{64}/<key>/' \
    | sed -E "s/(artifact-cache: (hit|miss)) [A-Za-z0-9_-]+/\\1 <target>/"
}

KIO_DEBUG_ARTIFACT_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>cold.err
KIO_DEBUG_ARTIFACT_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>warm.err

perl -0pi -e 's/print\("hi\\n"\(Str\)\)/print("bye\\n"(Str))/' m.kio
KIO_DEBUG_ARTIFACT_CACHE=1 "$KIO_BIN" build "$KIO_TARGET" 2>edit.err

printf 'cold cache log:\n'
probe cold.err
printf 'warm cache log:\n'
probe warm.err
printf 'edit cache log:\n'
probe edit.err
