#!/bin/sh
# Exercises multi-layer flat-call elaboration (specs/language.md
# § 4c): one application that spans two consecutive curry layers,
# `foo(A, A.mk_a(()), B, B.mk_b(()))`. The build block routes through
# the kio-prime-roundtrip per-case check, which lowers the package to
# Kio' and re-typechecks it; each layer's type argument must be
# assigned from that layer's binder (layer 1 → `A`, layer 2 → `B`),
# so the lowered Kio' preserves `foo(A, …, B, …)`.
set -u
cd workdir || exit
"$KIO_BIN" check
