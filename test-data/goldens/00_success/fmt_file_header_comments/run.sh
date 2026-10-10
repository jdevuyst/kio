#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
// file note
/// Module documentation.
module // keyword
app // slash
/ // segment
main // terminator
;
/// Function documentation.
fn f() { () }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp pkg.pkg.kio "$work/pkg.pkg.kio"
"$KIO_BIN" fmt "$work/pkg.pkg.kio" >/dev/null
"$KIO_BIN" fmt --check "$work/pkg.pkg.kio" >/dev/null

for comment in 'file note' keyword slash segment terminator; do
  test "$(printf '%s\n' "$formatted" | grep -F -c "// $comment")" -eq 1
done
for comment in 'package name' 'package terminator' 'build opener' 'cache value' 'cache interior' 'docs opener' 'md value' 'string chunk' 'support value' 'md_out value' 'html value' 'target value' 'bridge opener'; do
  test "$(grep -F -c "// $comment" "$work/pkg.pkg.kio")" -eq 1
done
grep -F 'md "docs";' "$work/pkg.pkg.kio" >/dev/null
grep -F 'out "out/rust/"' "$work/pkg.pkg.kio" >/dev/null
printf '%s\n' 'file-header and configuration comments preserved'
