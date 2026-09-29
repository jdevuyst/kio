#!/bin/sh
# Subject: type-args on the CPS projector apply
# (specs/formal/elaboration.md § 7.5 + § 9 IR-preservation
# contract for projector type-args).
#
# At a CPS-projector call, the receiver's type-args (the outer
# call's frame universals after substitution) must survive the
# projector application so later phase consumers retain the selected
# receiver-frame universals rather than declaration-local binders.
#
# A two-universals newtype `Pair[A][B] <U> : A & B & U`
# exercises the multi-binder case: each apply carries both `A` and
# `B`, in order, on its IR variant. Companion exec golden:
# exec_existential_cps_projector_apply (single universal,
# end-to-end). Companion canonical: exec_cps_projector_polymorphic
# (subject for the type-arg-through-codegen path).
set -u
cd workdir || exit
"$KIO_BIN" check
