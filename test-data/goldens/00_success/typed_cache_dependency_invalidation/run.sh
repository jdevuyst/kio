#!/bin/sh
# Typed-module cache cross-module dependency invalidation. The typed
# cache keys each module by its own source fingerprint *plus* the
# fingerprints of the same-package modules it depends on
# (`TypedModuleDependency::SamePackageModule`). Editing a dependency's
# public surface must re-typecheck the dependent even though the
# dependent's own bytes are unchanged.
#
# Module `b` uses `helper` from module `a`. Three steps:
#   1. cold check — both modules miss (cache empty) and write.
#   2. edit `b`'s own body — `a` HITs (its source and dependencies are
#      unchanged), `b` misses on its own source change. Baseline:
#      `a`'s typed entry survives an edit confined to `b`.
#   3. edit `a`'s public surface — `a` misses on its own source change,
#      and `b` misses too, on a *deps* mismatch: `a`'s fingerprint, a
#      dependency folded into `b`'s key, moved. Step 2's `a` hit versus
#      step 3's `b` deps-miss is the firing coverage for the dependency
#      fingerprint.
#
# The probe is gated on KIO_DEBUG_TYPED_CACHE. Content-addressed entries
# report a missing key rather than opening a stable-path entry and reporting
# which header field changed, so the test compares the exact keys directly:
# the unchanged dependency keeps its key, while the unchanged consumer moves
# only after its dependency's public surface moves.
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

probe() {
  grep -E '^typed-cache: (hit|miss) ' "$1" \
    | sed -E 's/[0-9a-f]{64}/<key>/g' \
    | sort
}

key_for() {
  awk -v module="$2" '$1 == "typed-cache:" && $4 == module { print $5; exit }' "$1"
}

# This case audits cold/warm cache transitions, so it must not inherit the
# harness's run-scoped cross-package typed-cache root.
KIO_DEBUG_TYPED_CACHE_ROOT='' KIO_DEBUG_TYPED_CACHE=1 KIO_DEBUG_WRITE_TYPED_CACHE=1 "$KIO_BIN" check 2>cold.err

perl -0pi -e 's/pub fn run\(\) -> \. \{ helper\(\) \}/pub fn run() -> . { let _ = (); helper() }/' b.kio
KIO_DEBUG_TYPED_CACHE_ROOT='' KIO_DEBUG_TYPED_CACHE=1 KIO_DEBUG_WRITE_TYPED_CACHE=1 "$KIO_BIN" check 2>edit_b.err

perl -0pi -e 's/pub fn helper\(\) -> \. \{ \(\) \}/pub fn helper2() -> . { () }\n\npub fn helper() -> . { helper2() }/' a.kio
KIO_DEBUG_TYPED_CACHE_ROOT='' KIO_DEBUG_TYPED_CACHE=1 KIO_DEBUG_WRITE_TYPED_CACHE=1 "$KIO_BIN" check 2>edit_a.err

cold_a=$(key_for cold.err a)
edit_b_a=$(key_for edit_b.err a)
edit_b_b=$(key_for edit_b.err b)
edit_a_a=$(key_for edit_a.err a)
edit_a_b=$(key_for edit_a.err b)

if [ -z "$cold_a" ] || [ "$cold_a" != "$edit_b_a" ]; then
  printf 'typed_cache_dependency_invalidation: unchanged dependency key moved\n' >&2
  exit 1
fi
if [ -z "$edit_b_b" ] || [ "$edit_b_b" = "$edit_a_b" ]; then
  printf 'typed_cache_dependency_invalidation: dependency edit did not move consumer key\n' >&2
  exit 1
fi
if [ -z "$edit_a_a" ] || [ "$edit_b_a" = "$edit_a_a" ]; then
  printf 'typed_cache_dependency_invalidation: source edit did not move dependency key\n' >&2
  exit 1
fi

printf 'cold cache log:\n'
probe cold.err
printf 'edit-b cache log:\n'
probe edit_b.err
printf 'edit-a cache log:\n'
probe edit_a.err
