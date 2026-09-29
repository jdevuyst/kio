#!/bin/sh
# Subject: `onto!` — surjection (specs/formal/elaborator.md § 3 — Soundness — onto!).
#
#     Γ ⊢ S ⇒_onto T ↪ e            S elaborates to T via onto!  glue e
#
# `onto!`'s rule subset is `iso!` plus two length-changing rules:
# R-Project-Prod (`A & B → A`, or any sub-product) and
# R-Collapse-Sum (`A | A → A`). The value mapping is a surjection
# — every target value has at least one source pre-image; the
# rules may identify multiple source values onto one target value.
#
# Two rule axes pinned here as onto!'s defining capabilities:
#   - R-Project-Prod: source product narrows to a sub-product.
#     `__fst__` and `__snd__` chains pick the kept components.
#   - R-Collapse-Sum: source sum with like-typed arms identifies
#     onto a single target arm. `__either__(s, id, id)` is the
#     canonical glue.
#
# The product-collapse case (`A & A → A`) falls out of
# R-Project-Prod composed with the source-order rule — not a
# separate rule. This case exercises both axes.
#
# Negative twin: 15_elaborator_error/onto_rejects_widening and
# 15_elaborator_error/onto_rejects_duplication — both pin the
# information-extending rules onto! excludes.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
