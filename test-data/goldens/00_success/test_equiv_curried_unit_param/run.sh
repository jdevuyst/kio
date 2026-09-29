#!/bin/sh
# Beta over consecutive unit-domain value groups (specs/formal/equiv.md
# § 2.1 β-Lam, one step per group; curry layers are distinct,
# specs/language.md § Function types): `pass_second(x)` saturates the
# first group only and suspends as the second-group closure
# `.(v: .) { v }`, and applying that closure substitutes the actual
# argument, so `pass_second(x)(mk()) ~ mk()` discharges (exit 0) with
# the stuck host action preserved. The case guards the per-layer step:
# an evaluator that force-runs the body with an implicit `()` for the
# unfed trailing group collapses the first application through both
# layers and leaves the second application stuck as the illegal
# residual `()(mk)` (a Stuck callee must be an atom or another stuck
# term, § 3) — a spurious exit 50 this golden trips on.
set -u
cd workdir || exit
"$KIO_BIN" test
