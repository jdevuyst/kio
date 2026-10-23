#!/bin/sh
# `narrow_sum!` requires type-set equality on non-`!` types — every
# non-`!` arm type in the source must appear in the target. Source
# `(A | B)`, target `(C)` — C is not in source, and B is not in
# target. Partial sum narrowing is `match!`'s territory, not
# narrow_sum!'s. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
