#!/bin/sh
# Subject: `align!` — payload-preserving sum-shape rearrangement
# (specs/formal/elaborator.md § 3 — Soundness — align!).
#
#     Γ ⊢ S ⇒_align T ↪ e            S elaborates to T via align! glue e
#
# `align!`'s rule subset is `iso!` plus sum-axis length-changing
# rules only: R-Widen-Sum (`A → A | B`) and R-Collapse-Sum
# (`A | A → A`). It deliberately excludes both product-shape-
# changing rules — R-Project-Prod (forbidden because projection
# drops a payload component, breaking payload preservation) and
# R-Diag-Prod (forbidden because duplication creates a payload
# the source did not supply).
#
# The form's semantic identity: every source DNF branch's
# product-slot payload survives unchanged. Only the sum-arm
# discrimination may coarsen (collapse) or extend (widen).
#
# Two rule axes pinned here:
#   - R-Widen-Sum: extend the sum with arms no source value can
#     reach. Payload of each source value untouched.
#   - R-Collapse-Sum: identify like-typed source arms onto a
#     single target arm. Payload of each source value untouched.
#
# Negative twin: 15_elaborator_error/align_rejects_projection and
# 15_elaborator_error/align_rejects_duplication — both pin the
# product-shape-changing rules align! excludes. Either of those
# would discard or fabricate payload.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
