#!/bin/sh
# Subject: E-FnSynth (specs/formal/elaboration.md § 4.2).
# A lambda literal in a synthesis-only position, where every
# value-parameter slot carries a concrete annotation. The rule
# lifts the user-written binders and annotations into the
# synthesized polytype.
#
#     ∀ᾱ. (A₁, …, Aₙ) → R′
#
# Every top-level `fn` declaration's body is itself in a
# synthesizing context (no parent has an expected polytype to
# push down). The header's value-param annotations and the
# explicit `[ᾱ]` binders together pin the synthesized scheme;
# the body's `R′` is then read off the body's synthesis (here,
# a trivial bare-variable reference).
#
# The discriminating shape vs. E-FnCheck: there is no surrounding
# polytype here — the polytype the rule synthesizes is exactly
# the one the user wrote in the header.
set -u
cd workdir || exit
"$KIO_BIN" check
