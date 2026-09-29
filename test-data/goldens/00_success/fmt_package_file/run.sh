#!/bin/sh
# Pin canonical `kio fmt` output for a package file's shape:
# - the `bridge` block lays each module glob on its own line in
#   declaration order (no reordering);
# - a leading line comment above the `package` directive survives the
#   round-trip.
#
# Strategy mirrors fmt_canonical: workdir keeps the canonical `build`
# block (so the harness build-block pre-flight sees its target) while
# the `bridge` block stays single-line minified — the package-file
# construct under test. Copied to scratch so `kio fmt` (which rewrites
# in place) doesn't canonicalise the tracked source.
set -u
if ! grep -Eq '^bridge[[:space:]]+\{[^}]+\}$' workdir/fmt_package_file.pkg.kio; then
  printf 'fixture must keep the bridge globs on one line\n' >&2
  exit 1
fi
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/fmt_package_file.pkg.kio "$work/fmt_package_file.pkg.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat fmt_package_file.pkg.kio
"$KIO_BIN" fmt >/dev/null || exit
cat fmt_package_file.pkg.kio
