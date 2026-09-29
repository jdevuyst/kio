#!/bin/sh
# Subject: existential-newtype construction (specs/language.md
# § Existential type binders — *Construction inference*; specs/
# formal/elaboration.md § 7.4 + § 5 Function application).
#
# The constructor scheme for a newtype with existential binders is
# extended with the existentials as additional type-params; the
# typer infers them from the payload's structure (§ 5.1, the same
# unification machinery that drives universal inference). The
# result type carries only the universals — `Pack(A)` for
# `Pack[A] <U> : A & U` constructed at `(A & U)`.
#
# Three variants: a one-universal-one-existential newtype, a
# multi-existential newtype, and a no-universals existential
# newtype. Each construction site omits the existential suffix and
# requires the payload type to determine every hidden witness.
set -u
cd workdir || exit
"$KIO_BIN" check
