#!/bin/sh
set -eu

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

mkdir "$scratch/pkg"
cp -R workdir/. "$scratch/pkg/"
cd "$scratch/pkg" || exit

KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >cold.out 2>cold.err
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >warm.out 2>warm.err
cmp cold.out warm.out >/dev/null

perl -0pi -e 's/fn a\(\) -> \. \{ \(\) \}/fn a() -> . { let _ = (); () }/' m.kio
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >edit.out 2>edit.err
"$KIO_BIN" test --no-cache >edit-cold.out 2>edit-cold.err
cmp edit.out edit-cold.out >/dev/null

perl -0pi -e 's/fn hidden\(\) -> \. \{ \(\) \}/fn hidden() -> . { let _ = (); () }/' helper_provider.kio
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >helper-edit.out 2>helper-edit.err
"$KIO_BIN" test --no-cache >helper-edit-cold.out 2>helper-edit-cold.err
cmp helper-edit.out helper-edit-cold.out >/dev/null

perl -0pi -e 's/pub type Shape = \.;/pub type Shape = . \& .;/' alias_provider.kio
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >alias-edit.out 2>alias-edit.err
"$KIO_BIN" test --no-cache >alias-edit-cold.out 2>alias-edit-cold.err
cmp alias-edit.out alias-edit-cold.out >/dev/null

perl -0pi -e 's/pub fn answer\(\) -> Bool \{ \.t\(Bool\) \}/pub fn answer() -> Bool { .f(Bool) }/' qualified_provider.kio
set +e
KIO_DEBUG_EQUIV_CACHE=1 "$KIO_BIN" test >qualified-edit.out 2>qualified-edit.err
qualified_cached_status=$?
"$KIO_BIN" test --no-cache >qualified-edit-cold.out 2>qualified-edit-cold.err
qualified_cold_status=$?
set -e
if [ "$qualified_cached_status" -ne "$qualified_cold_status" ]; then
  printf 'qualified mutation status diverged: cached=%s no-cache=%s\n' \
    "$qualified_cached_status" "$qualified_cold_status" >&2
  exit 1
fi
if [ "$qualified_cached_status" -ne 50 ]; then
  printf 'qualified mutation unexpectedly passed: status=%s\n' "$qualified_cached_status" >&2
  exit 1
fi
if ! cmp qualified-edit.out qualified-edit-cold.out >/dev/null; then
  printf 'qualified mutation output diverged between cached and no-cache runs\n' >&2
  exit 1
fi

printf 'cold/warm stdout matched\n'
printf 'warm cache log:\n'
sed -E 's/ [0-9a-f]{64} / <key> /' warm.err | sort
printf 'function edit cached/no-cache stdout matched\n'
printf 'edit cache log:\n'
sed -E 's/ [0-9a-f]{64} / <key> /' edit.err | sort
printf 'private-helper edit cached/no-cache stdout matched\n'
printf 'private-helper edit cache log:\n'
sed -E 's/ [0-9a-f]{64} / <key> /' helper-edit.err | sort
printf 'alias edit cached/no-cache stdout matched\n'
printf 'alias edit cache log:\n'
sed -E 's/ [0-9a-f]{64} / <key> /' alias-edit.err | sort
printf 'qualified-alias/local edit cached/no-cache failure matched\n'
printf 'qualified-alias/local edit cache log:\n'
sed -E 's/ [0-9a-f]{64} / <key> /' qualified-edit.err | sort
