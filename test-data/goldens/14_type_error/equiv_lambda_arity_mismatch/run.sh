#!/bin/sh
# `equiv` arm bodies must share a type. Two closures whose binder
# lists differ in arity — `.(x)` vs `.(x, y)` — synthesize the
# distinct function types `. -> .` and `(. & .) -> .`, so
# `kio test` fails at the type-check step (exit 14, the type-error
# category) before the partial evaluator ever compares NFs. This is
# the equiv-context analogue of the closure-arity mismatch that
# specs/formal/equiv.md § 4.5 would otherwise express as
# `Stuck(f, [a]) ≄ Stuck(f, [a, b])`: a differing-arity comparison
# can never reach the NF stage because the typer rejects it first.
set -u
cd workdir || exit
"$KIO_BIN" test
