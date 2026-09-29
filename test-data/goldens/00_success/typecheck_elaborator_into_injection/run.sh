#!/bin/sh
# Subject: `into!` — injection (specs/formal/elaborator.md § 3 — Soundness — into!).
#
#     Γ ⊢ S ⇒_into T ↪ e            S elaborates to T via into!  glue e
#
# `into!`'s rule subset is `iso!` plus two length-changing rules:
# R-Widen-Sum (`A → A | B`) and R-Diag-Prod (`A → A & A`). The
# value mapping is an injection — every source value maps to a
# unique target value, and the target may carry slack values not
# in the image.
#
# Two rule axes pinned here as into!'s defining capabilities:
#   - R-Widen-Sum: source widens to a sum that contains its type
#     (slack on the right). The kth source DNF branch of type T
#     maps deterministically to the kth target slot of type T.
#   - R-Diag-Prod: source maps to the diagonal of a product whose
#     slots are all the source type. `λa. (a, a)` is injective.
#
# These are the two rules `iso!` excludes that `into!` adds; the
# rest of `into!`'s rule set is iso!-shared (R-Comm, R-Identity,
# R-Distrib) and not exercised here — see
# typecheck_elaborator_iso_bijection for those.
#
# Negative twin: 15_elaborator_error/ease_rejects_duplication and
# 15_elaborator_error/align_rejects_duplication — both pin that
# R-Diag-Prod is `into!`-only. 15_elaborator_error/onto_rejects_widening
# pins that R-Widen-Sum is also `into!`-only among the surjection
# / projection forms.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
