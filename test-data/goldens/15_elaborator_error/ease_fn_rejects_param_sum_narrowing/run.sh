#!/bin/sh
# `ease!`'s function-arrow walk over `->` rejects parameter-sum-
# narrowing. Source `A -> .`, target `(A | B) -> .`: the call
# site wants a *wider* parameter than the source accepts, so the
# runtime would have to dispatch a `B` value into a function that
# only accepts `A`. The leaf coercion sees swapped `(A | B, A)`,
# needs `A | B → A` (sum narrowing), which is not in `ease!`'s
# rule subset (sum narrowing is partial — that case belongs to
# `match!`). Exit 15.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
