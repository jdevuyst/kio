#!/bin/sh
# Regression guard for the dropped-`//`-file-header bug: a plain `//`
# file-header comment above a module directive must survive `kio fmt`.
# Each source here leads with one -- a root `module`, a `module`
# submodule carrying its own leading comment, and the `package`
# interface. The canonical form already on disk is `--check`-clean,
# so `kio fmt --check` reports nothing and exits 0; the headers also
# don't disturb the build, which we then exercise end-to-end.
# The per-case fmt-canonical check independently re-verifies every
# file under workdir/. Pairs with `60_fmt_check/fmt_check_dirty` (exit 60).
set -u
cd workdir || exit
"$KIO_BIN" fmt --check . || exit
"$KIO_BIN" build "$KIO_TARGET" || exit
"$KIO_RUNNER" --protocol testapi-print out/"$KIO_TARGET"
