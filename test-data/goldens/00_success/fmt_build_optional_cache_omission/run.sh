#!/bin/sh
# `kio fmt` preserves omission of the optional `cache` field while
# expanding inline target blocks and preserving their written paths.
# A second round pins idempotence. Explicit `cache ()` preservation is
# covered separately by fmt_named_entry_order.
#
# Strategy mirrors fmt_package_file: copy the deliberately non-canonical
# source to scratch so `kio fmt` (which rewrites in place) doesn't
# canonicalise the tracked file.
set -u
if grep -Eq '(^|[;{}[:space:]])cache[[:space:]]' workdir/app.pkg.kio ||
   [ "$(grep -Ec '^  target (js|ts) \{.*\};?$' workdir/app.pkg.kio)" -ne 2 ]; then
  printf 'fixture must omit cache and keep both target blocks inline\n' >&2
  exit 1
fi
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/app.pkg.kio "$work/app.pkg.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat app.pkg.kio
"$KIO_BIN" fmt >/dev/null || exit
cat app.pkg.kio
