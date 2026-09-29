#!/bin/sh
# `narrow_prod!` only drops slots — the target's multiset must be a
# sub-multiset of the source's. Source `(A & B)`, target `(A & B & C)`
# — the target has an extra `C` slot. narrow_prod! does not
# synthesize factors; widening is `widen_prod!`'s job. Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
