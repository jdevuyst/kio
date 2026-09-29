#!/bin/sh
# Subject: E-FnCheck (specs/formal/elaboration.md § 4.1).
# A lambda literal checked against an expected polytype
# `∀ᾱ. (T₁, …, Tₙ) → R`. The rule:
#   - skolemizes the leading binders ᾱ,
#   - fills in each value-param's type from Tᵢ (the surface slot
#     may be absent, `_`, or syntactically ≡ Tᵢ),
#   - checks the body against R under the skolemized Γ.
#
# Each caller below sits in a checking position; its `fn`-literal
# value-arg has a different annotation shape — every slot
# absent, every slot `_`-marked, every slot syntactically ≡ Tᵢ —
# all admissible under E-FnCheck. The expected type comes from
# the receiving parameter slot's polytype.
set -u
cd workdir || exit
"$KIO_BIN" check
