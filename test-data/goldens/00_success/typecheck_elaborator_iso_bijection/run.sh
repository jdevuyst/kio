#!/bin/sh
# Subject: `iso!` — bijection (specs/formal/elaborator.md § 3 — Soundness — iso!).
#
#     Γ ⊢ S ⇒_iso T ↪ e            S elaborates to T via iso!  glue e
#
# `iso!`'s rule subset is exactly the iso!-shared rules:
# R-Assoc, R-Distrib, R-Comm (source-order-pinned), and the two
# identity-absorption rules (R-Identity-Prod, R-Identity-Sum).
# It admits no length-changing rule — no R-Widen-Sum, no
# R-Diag-Prod, no R-Project-Prod, no R-Collapse-Sum. The value
# mapping is a bijection on the value-set of `S` and `T`.
#
# Three rule axes pinned here:
#   - R-Comm: rearranging product slots and sum arms at distinct
#     slot types.
#   - R-Identity-Prod: `A & . ↔ A` in both directions.
#   - R-Identity-Sum: `A | ! ↔ A` in both directions.
#   - R-Distrib: `A & (B | C) ↔ (A & B) | (A & C)` (DNF/CNF
#     interchange).
#
# Negative twin: 15_elaborator_error/iso_rejects_widening,
# 15_elaborator_error/iso_rejects_collapse,
# 15_elaborator_error/iso_rejects_projection, and
# 15_elaborator_error/iso_rejects_duplication — each pins one of the
# four length-changing rules iso! excludes.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
