#!/bin/sh
# Subject: E-Unit (specs/formal/elaboration.md § 7.1).
#
#   ──────────────────
#   Γ ⊢ () ⇑ () ↪ ()
#
# The unit value `()` is the simplest synthesizing form: no
# context lookup, no expected-type interaction, no role
# admission. Each fn below has a body that is literally `()` in
# a different syntactic context — body of a fn, RHS of a let,
# value-arg of an intrinsic call, return slot of a polymorphic
# instantiation. The rule fires uniformly.
set -u
cd workdir || exit
"$KIO_BIN" check
