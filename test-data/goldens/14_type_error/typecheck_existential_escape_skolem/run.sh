#!/bin/sh
# Subject: existential-skolem escape rejection (specs/formal/
# elaboration.md § 4.1 E-FnCheck escape check; § 7.5 Existential
# newtype CPS projector).
#
# An existential-bearing newtype's CPS projector hands the payload
# to a continuation under a fresh skolem. The escape check rejects
# any body whose synthesized type mentions the skolem outside the
# continuation's scope — the witnesses cannot leak into the
# projector's result type. The case constructs exactly that
# escape: the continuation returns `__snd__(A, U, pair) : U` while
# the outer call's result type is `A`. The typer must surface a
# structured user error (exit 14), not a panic.
set -u
cd workdir || exit
"$KIO_BIN" check
