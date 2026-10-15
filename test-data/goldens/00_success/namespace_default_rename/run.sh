#!/bin/sh
set -eu

# The .input file deliberately omits namespace. Formatting must preserve that
# choice through a package rename; explicit strings must instead stay pinned.
# Each artifact is exercised through the independent public-interface runner.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp -R workdir/. "$work/"
cp omitted.input "$work/before.pkg.kio"
cd "$work"
mv testapi.input testapi.kio
mv testapi/io.input testapi/io.kio
mv testapi/main.input testapi/main.kio

"$KIO_BIN" test >/dev/null
"$KIO_BIN" build "$KIO_TARGET" >/dev/null
"$KIO_RUNNER" --package-name before --protocol testapi-print "out/$KIO_TARGET"

"$KIO_BIN" fmt before.pkg.kio >/dev/null
if grep -q 'namespace' before.pkg.kio; then
  printf 'formatting inserted an omitted namespace\n' >&2
  exit 1
fi
# Write explicit Unit into each target and rename the package. Unit keeps the
# same dynamic selection as omission, and formatting must retain it.
sed 's/package before;/package after;/' before.pkg.kio |
  awk '/    out / { sub(/"$/, "\";"); print; print "    namespace ();"; next } { print }' > after.pkg.kio
rm before.pkg.kio
"$KIO_BIN" fmt after.pkg.kio >/dev/null
test "$(grep -c 'namespace ()' after.pkg.kio)" -eq 8
"$KIO_BIN" fmt --check after.pkg.kio >/dev/null
"$KIO_BIN" build "$KIO_TARGET" >/dev/null
"$KIO_RUNNER" --package-name after --protocol testapi-print "out/$KIO_TARGET"

# Preserve each host's namespace grammar while pinning a different name from
# both the old and the new package name.
case "$KIO_TARGET" in
  swift|haskell) pinned=Pinned ;;
  *) pinned=pinned ;;
esac
sed "s/namespace ()/namespace \"$pinned\"/g; s/package after;/package renamed;/" \
  after.pkg.kio > renamed.pkg.kio
rm after.pkg.kio
cp renamed.pkg.kio pinned.expected
"$KIO_BIN" fmt renamed.pkg.kio >/dev/null
cmp pinned.expected renamed.pkg.kio
"$KIO_BIN" build "$KIO_TARGET" >/dev/null
"$KIO_RUNNER" --package-name renamed --artifact-namespace "$pinned" \
  --protocol testapi-print "out/$KIO_TARGET"
