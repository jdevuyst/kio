#!/bin/sh
# `narrow_prod!` rejects sum narrowing — `(A | B) -> A` is not a
# product-axis coercion (it's a partial operation that belongs to
# `match!` or, with constraints, to `narrow_sum!`). Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
