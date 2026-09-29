#!/bin/sh
# A monomorphic `fn` cannot satisfy a polymorphic expected type.
# `higher` declares its parameter as `[U] U -> U` (rank-2). The
# call passes `.(y) { y }` (no `[…]` binder), so the typer reports
# a type-binder-arity mismatch and exits 14.
set -u
cd workdir || exit
"$KIO_BIN" check
