#!/bin/sh
# Subject: E-Let (specs/formal/elaboration.md § 6).
#
#     Γ ⊢ e ⇑ T ↪ e′
#     Γ, x : T ⊢ rest ⇑ U ↪ rest′
#   ─────────────────────────────────
#   Γ ⊢ let x = e ; rest ⇑ U ↪ let x = e′ ; rest′
#
# Three rule properties pinned here:
#   - No annotation slot: the bound name has no `: T` syntax.
#   - The RHS is synthesized in pure ⇑ mode — no expected type
#     from the body flows backward into it.
#   - No generalization: the bound type is precisely the RHS's
#     synthesized type, not a generalized scheme.
#
# Positive twin: 00_success/typecheck_polytype_let_alias pins
# that a polymorphic path expression on the RHS — anchored at a
# top-level polymorphic `fn` — propagates its polytype to the
# local. No generalization happens; the polytype was already
# synthesized.
#
# Negative twin: 14_type_error/fn_param_no_backward_flow pins
# that an un-annotated lambda literal `let f = .(x) { x };` (no
# expected polytype, no call-site context) still rejects —
# Hindley-Milner-style let-generalization stays excluded.
set -u
cd workdir || exit
"$KIO_BIN" check
