#!/bin/sh
# Subject: existential newtype open / unpack (specs/formal/
# elaboration.md § 7.5 Existential newtype CPS projector; § 4.1
# E-FnCheck skolemization).
#
# The CPS projector's continuation is checked against
# `∀U. (Payload[A..., U]) → R`. E-FnCheck skolemizes the leading
# `[U]` binder, types the body under the skolemized Γ, and runs
# the escape check at the binder boundary. The two functions
# here exercise the open: a single-skolem unpack returning the
# universal component, and a skolem-reachable-but-discarded body
# returning `()`. Both stay within the continuation's scope —
# the escape check passes.
set -u
cd workdir || exit
"$KIO_BIN" check
