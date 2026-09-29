#!/bin/sh
# Subject: existential newtype with a polymorphic-arrow payload
# (specs/language.md § Existential type binders; specs/formal/
# elaboration.md § 7.5 Existential newtype CPS projector + § 2
# polytypes — arrows admit polytype slots).
#
# When the payload references a polymorphic arrow (`[R], S -> R) ->
# R`), the existential `<S>` lifts to a universal at the synthesized
# constructor scheme — `[S] ([R](S -> R) -> R) -> Wrapped` — and
# the inner `[R]` stays as a polytype slot inside the payload. The
# projector's continuation receives the same shape under the
# continuation's skolemized `[S]`; instantiating the inner `[R]`
# happens at the call site inside the continuation.
#
# Three calls exercise the rule: construction at the outer frame,
# discarding consumption inside the continuation, and forced
# `. -> .` instantiation of the rank-2 payload.
set -u
cd workdir || exit
"$KIO_BIN" check
