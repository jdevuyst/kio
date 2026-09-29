#!/bin/sh
# Subject: `ease!` — no-duplication structural coercion
# (specs/formal/elaborator.md § 3 — Soundness — ease!).
#
#     Γ ⊢ S ⇒_ease T ↪ e            S elaborates to T via ease! glue e
#
# `ease!`'s rule subset is `iso!` plus every sum-axis and every
# product-axis length-changing rule *except* R-Diag-Prod. The
# form admits R-Widen-Sum, R-Project-Prod, R-Collapse-Sum in a
# single coercion — covering the gap neither `align!` (no
# projection) nor `onto!` (no widening) alone covers.
#
# The form's semantic identity: **structural coercion without
# value duplication**. Every source factor is consumed at most
# once along the elaboration; the engine never emits the
# diagonal `λa. (a, a)`.
#
# Three rule axes pinned here, including the union no other
# algebraic form admits in one coercion:
#   - R-Widen-Sum + R-Project-Prod in one coercion: the
#     "ease! gap" — source `A & B`, target `A | C` requires
#     project-then-widen. `align!` rejects projection;
#     `onto!` rejects widening.
#   - R-Project-Prod alone: drop a product slot. Inherited
#     from `onto!`'s rule set.
#   - R-Collapse-Sum alone: identify like-typed arms.
#     Inherited from both `onto!` and `align!`.
#
# Negative twin: 15_elaborator_error/ease_rejects_duplication —
# pins R-Diag-Prod is `into!`-only and `ease!` excludes it
# specifically to preserve the no-duplication identity.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
