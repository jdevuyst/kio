#!/bin/sh
# Two stuck host calls with the same argument but different head
# functions. Each arm residualizes to `Stuck(callee, [()])`; the
# callees differ (`f` vs `g`), so the two NFs are distinguished by
# the callee-comparison branch of the equivalence relation
# (specs/formal/equiv.md § 4.3 structural recursion on the
# application skeleton; § 4.5 "different head"). This is the
# worked example in § 5.3. Distinct from `equiv_terms_diverge`,
# whose arms are bare `Atom`s (zero-arg host calls) rather than
# `Stuck` applications. Exits 50 from the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
