#!/bin/sh
# Subject: E-Named (specs/formal/elaboration.md § 3).
# A registry-keyed identifier — top-level fn, intrinsic, or
# elaborator name — synthesizes its scheme from the named-callable
# registry rather than from the local Γ.
#
#     (name ↦ scheme(name, Γ_roles)) ∈ Registry
#   ──────────────────────────────────────────────
#   Γ ⊢ name ⇑ scheme(name, Γ_roles) ↪ name
#
# Each fn body is one E-App on a registry-keyed callee. The
# scheme is fixed by the name, not by Γ — that is what E-Named
# pins. Covers the three registry categories: top-level fn,
# intrinsic (`__fst__`), and elaborator name (`__iso__` reached
# via its `iso!` surface form would route through the same
# rule). The intrinsic path is the discriminating case: the
# call is well-typed only when `__fst__`'s registry scheme
# `[A][B](p: A & B) -> A` is in scope, since no local
# binder gives it.
set -u
cd workdir || exit
"$KIO_BIN" check
