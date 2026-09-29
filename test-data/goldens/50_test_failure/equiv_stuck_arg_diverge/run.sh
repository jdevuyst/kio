#!/bin/sh
# Two stuck host calls with the same head function `f` but different
# Unit-saturated residual arguments: `a(())` versus `b(())`. The arms
# residualize as `f(a(()))` and `f(b(()))`; the outer callees match, so
# the distinction comes from the per-argument structural-recursion branch
# of the equivalence relation
# (specs/formal/equiv.md § 4.3, args compared positionally).
# Complements `equiv_stuck_head_diverge`, which distinguishes on
# the callee instead. Exits 50 from the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
