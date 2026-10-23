#!/bin/sh
# A local elaborator implementation can call a pure helper through a qualified
# import. A stable-signature helper-body edit must invalidate the elaborator's
# module, while an unchanged warm run must hit both typed entries.
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

KIO_DEBUG_TYPED_CACHE_ROOT='' KIO_DEBUG_TYPED_CACHE=1 KIO_DEBUG_WRITE_TYPED_CACHE=1 \
  "$KIO_BIN" check 2>cold.err

printf '\n' >>typedcache_local_elab.pkg.kio
KIO_DEBUG_TYPED_CACHE_ROOT='' KIO_DEBUG_TYPED_CACHE=1 KIO_DEBUG_WRITE_TYPED_CACHE=1 \
  "$KIO_BIN" check 2>warm.err

sed 's/{ __term_unit__(ct) }/{ let result = __term_unit__(ct); result }/' \
  provider.kio >provider.next
mv provider.next provider.kio
KIO_DEBUG_TYPED_CACHE_ROOT='' KIO_DEBUG_TYPED_CACHE=1 KIO_DEBUG_WRITE_TYPED_CACHE=1 \
  "$KIO_BIN" check 2>edited.err

cold_main=$(key_for cold.err main)
warm_main=$(key_for warm.err main)
edited_main=$(key_for edited.err main)
warm_provider=$(key_for warm.err provider)
edited_provider=$(key_for edited.err provider)

if [ -z "$cold_main" ] || [ "$cold_main" != "$warm_main" ]; then
  printf 'typed_cache_local_elaborator_qualified_helper_invalidation: package-only edit moved main key\n' >&2
  exit 1
fi
if [ -z "$edited_main" ] || [ "$warm_main" = "$edited_main" ]; then
  printf 'typed_cache_local_elaborator_qualified_helper_invalidation: helper body edit did not move main key\n' >&2
  exit 1
fi
if [ -z "$edited_provider" ] || [ "$warm_provider" = "$edited_provider" ]; then
  printf 'typed_cache_local_elaborator_qualified_helper_invalidation: helper source edit did not move provider key\n' >&2
  exit 1
fi

printf 'cold cache log:\n'
probe cold.err
printf 'warm cache log:\n'
probe warm.err
printf 'edited-helper cache log:\n'
probe edited.err
