#!/bin/sh
# Two unit-domain closures with the same binder shape but distinct
# bodies: the identity `.(y: .) { y }` vs the constant `.(y: .) { () }`.
# Closure equivalence renames the binder to a shared fresh opaque atom
# (specs/formal/equiv.md § 4.1; § 4.4 admits no unit exception), so the
# bodies compare as `y` vs `()` — distinct. Probing the pair at the
# literal `()` instead would equate them and break congruence: applied
# to a stuck unit-typed action (§ 2.4 / § 3 residuals), the identity
# keeps the action while the constant discards it, so the closures act
# differently on a value their domain admits. Exits 50 from the `5x`
# test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
