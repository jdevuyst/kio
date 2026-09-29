#!/bin/sh
# Subject: `atom!` — focused single-arm picker
# (specs/formal/elaborator.md § 3 — Soundness — atom!).
#
#     Γ ⊢ S ⇒_atom T ↪ e            S elaborates to T via atom! glue e
#
# `atom!`'s rule subset is `onto!`'s (R-Project-Prod +
# R-Collapse-Sum, plus iso!-shared) but with the structural
# pre-condition R-Single-Arm: `T` must DNF-normalize to a
# **single arm** (atomic, nominal, or product). Multi-arm
# sum targets are rejected at the pre-condition before any rule
# fires. The source must also be non-atomic — `atom!` is a focused
# picker over an aggregate.
#
# The form's semantic identity: pull one specific value of type
# `T` out of an aggregate. The four-pass per-source-branch
# preference order degenerates because there is exactly one
# target arm to consider; the result is unique by construction.
#
# Two rule axes pinned here:
#   - R-Project-Prod against a single-arm target: source product
#     narrows to the unique target type. The picked component is
#     fixed by source-order.
#   - R-Collapse-Sum across a uniform sum: each source arm
#     projects (R-Project-Prod) or matches the unique target
#     type, then the like-typed arms collapse onto the singleton.
#
# Negative twin: 15_elaborator_error/atom_rejects_multi_arm_target —
# pins the R-Single-Arm pre-condition: target `A | B` is a
# 2-arm sum, rejected before rule search begins.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
