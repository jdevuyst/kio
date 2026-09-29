#!/bin/sh
# Pin canonical `kio fmt` output for `import` shapes:
# - Two canonical blocks (`import __intrinsics__;` then every other
#   `import`) separated by exactly one blank line.
# - Within each block, ASCII codepoint sort on the rendered text.
# - Within a selective form, names sort by ASCII codepoint.
# - Selective forms keep the provider before a parenthesized name list.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
# Preserve the declared module suffix while formatting a scratch copy.
mkdir -p "$work/fmt_use" || exit
cp workdir/fmt_use/main.kio "$work/fmt_use/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt fmt_use/main.kio >/dev/null || exit
cat fmt_use/main.kio
"$KIO_BIN" fmt fmt_use/main.kio >/dev/null || exit
cat fmt_use/main.kio
