#!/bin/sh
# Two closures with the same binder shape but distinct bodies:
# `.(x) { f(x) }` vs `.(x) { g(x) }`. After α-renaming the bound
# parameter to a shared fresh atom (specs/formal/equiv.md § 4.1),
# the bodies reduce to `Stuck(f, [x])` vs `Stuck(g, [x])`, which
# differ on the head atom. So α-equivalence does NOT rescue them:
# distinct free functions in the body keep the closures apart.
# (η would rewrite each arm to `f` / `g` respectively — still
# distinct.) Exits 50 from the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
