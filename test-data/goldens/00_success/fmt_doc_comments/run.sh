#!/bin/sh
# Tests that `kio fmt` round-trips doc-comments (`///`) on every
# attachment point: fn, type, alias, newtype, labels, op (module body),
# host type, host fn (root module), plus the module-level form. A
# regular `//` line comment passes through unchanged (`////` and longer
# are now a lex error, not a ruler).
#
# Strategy: workdir/ carries the human-readable input form. The test
# copies it to a scratch directory, runs `kio fmt`, and cats the three
# kio files (root module, submodule, then the package file). Two rounds
# confirm idempotence.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp -r workdir/. "$work/" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat fmt_doc_comments.kio
cat fmt_doc_comments/main.kio
cat fmt_doc_comments.pkg.kio
"$KIO_BIN" fmt >/dev/null || exit
cat fmt_doc_comments.kio
cat fmt_doc_comments/main.kio
cat fmt_doc_comments.pkg.kio
