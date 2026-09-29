#!/bin/sh
# Subject: E-App (specs/formal/elaboration.md § 5) — fully
# explicit type-arguments at every call site.
#
#     Γ ⊢ f ⇑ ∀ᾱ. (T₁, …, Tₙ) → R
#     explicit type-args  U₁, …, U_|ᾱ|  (every binder pinned)
#     each aᵢ ⇓ Tᵢ[ᾱ := Ū]
#   ──────────────────────────────────────────────────────
#   Γ ⊢ f(Ū, a₁, …, aₙ) ⇑ R[ᾱ := Ū]
#
# Every call below writes out every type-argument explicitly —
# no `_` placeholders, no fully-elided slots. Discriminating
# vs. typecheck_underscore_marked_type_args (partial) and
# typecheck_return_type_inference (fully elided): this golden
# pins the m = |ᾱ| boundary.
set -u
cd workdir || exit
"$KIO_BIN" check
