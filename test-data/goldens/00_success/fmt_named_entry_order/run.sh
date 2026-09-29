#!/bin/sh
# Named entries accept any order; formatting alone selects their canonical
# layout. Leading/trailing separators preserve comments once across every file
# kind. Pin the complete output and verify a second format is inert.
# These are syntax/formatting fixtures, not dependency-fetch or typecheck inputs.
set -u
fixture_root=$(mktemp -d) || exit
trap 'rm -rf "$fixture_root"' 0 1 2 15
cp -R workdir/. "$fixture_root" || exit
cd "$fixture_root" || exit
failed=0
for fixture in fmt_named_entry_order.pkg.kio fmt_named_entry_order/main.kio lib.dep.kio lib.lock.kio app.sig.kio; do
  if ! "$KIO_BIN" fmt "$fixture" >/dev/null; then
    failed=1
    continue
  fi
  cp "$fixture" "$fixture.first" || exit
  if ! "$KIO_BIN" fmt "$fixture" >/dev/null; then
    failed=1
    continue
  fi
  if ! cmp "$fixture.first" "$fixture"; then
    failed=1
  fi
  cat "$fixture" || exit
done
exit "$failed"
