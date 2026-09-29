#!/bin/sh
# Subject: E-Var (specs/formal/elaboration.md § 3).
# A local binder synthesizes its bound type from Γ.
#
#     (x : T) ∈ Γ
#   ───────────────
#   Γ ⊢ x ⇑ T ↪ x
#
# Each fn here has a body that is exactly a single bound name —
# a value-param reference, a let-bound reference, and a re-bound
# alias chain. The body's synthesized type must agree with the
# declared return type, which is the only thing pinning each
# variable's type at the use site.
set -u
cd workdir || exit
"$KIO_BIN" check
