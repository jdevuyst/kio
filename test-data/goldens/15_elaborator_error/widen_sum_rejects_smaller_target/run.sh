#!/bin/sh
# `widen_sum!` extends the target — source's arm-type multiset must
# be a sub-multiset of target's. Source `(A | B)`, target `(A)` —
# B is in source but not target, so this is narrowing, not widening.
# Narrowing is `narrow_sum!` (or `match!`). Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
