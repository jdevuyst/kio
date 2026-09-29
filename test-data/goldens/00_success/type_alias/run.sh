#!/bin/sh
# A `type` declaration names a structural type expression.
# The typer unfolds a type alias structurally — `Small` resolves
# to `I32`, the parametric `Pair(A, B)` to `(A & B)` — so the
# identity functions below typecheck against the unfolded shapes.
set -u
cd workdir || exit
"$KIO_BIN" check
