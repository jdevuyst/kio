#!/bin/sh
# Subject: `atom!`'s target slot — surrounding expected type
# pins `T` before source-driven inference fires.
# (specs/language.md:100 — `atom!` § type-arg-inference path).
#
#     fn pick_a(p: A & B) -> A { atom!(p) }
#
# The source `A & B` has two candidate components (`A` and `B`)
# that both appear in every DNF arm. The source-driven fallback
# (`infer_atom_target`) reports "multiple distinct types … the
# target is ambiguous" for such inputs. The fix is that the typer
# consults the surrounding expected type *before* the source-
# driven path: the function's `-> A` (or `-> B`) return slot
# pins `T`, then the algebraic engine runs with target `T` and
# elaborates via R-Project-Prod (`atom!`'s rule subset).
#
# This is the "expected type fills the trailing target slot"
# branch of the resolution order — applies uniformly across the
# three target-driven kinds (`atom!`, `one_sum!`, `one_prod!`);
# the other 14 elaborator kinds already errored cleanly when both
# slots were absent.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
