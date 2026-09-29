#!/bin/sh
# Beta over a unit-domain value group (specs/formal/equiv.md § 2.1
# β-Lam; specs/formal/prime.md § 3 simultaneous substitution). A unary
# group substitutes the actual argument — `(.(x: .) { e })(v)` reduces
# to `e[x := v]` for every unit-typed argument residual: an equiv
# parameter's fresh opaque atom (§ 4.4 admits no unit exception) and a
# stuck host call alike, not only the literal `()`. A nullary group has
# no binder, so its one-slot unit packet is discarded — `thunk(v)`
# reduces to `thunk`'s body for the same argument residuals. So
# `wrap(x) ~ x`, the direct-lambda spelling, `wrap(mk()) ~ mk()`,
# `thunk(x) ~ ()`, and `thunk(mk()) ~ ()` all discharge (exit 0). The
# stuck blocks pin the two directions apart: a unary binder must
# substitute `mk()` (not collapse it to `()`), and a nullary group must
# discard it. The case guards the binding step: an evaluator that
# consumes a unit-domain group's argument only when it is the literal
# `()` hardwires the binder to `()` and stuck-applies the leftover
# argument onto the body's residual — the illegal `()(x)` residual (a
# Stuck callee must be an atom or another stuck term, § 3) and a
# spurious exit 50 this golden trips on. The curried sibling shape
# lives in test_equiv_curried_unit_param.
set -u
cd workdir || exit
"$KIO_BIN" test
