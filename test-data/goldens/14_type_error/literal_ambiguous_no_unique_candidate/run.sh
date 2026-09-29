#!/bin/sh
# Tier 4 of literal resolution (specs/language.md § Literals): when
# no annotation (tier 1) and no expected type (tier 2) apply, and
# the package declares more than one role-bearing type whose
# shape the literal admits (tier 3 finds no *unique* candidate),
# the literal is a type error. Here `100` sits in a `let` right-
# hand side — a synthesis position — and the package exports two
# int-shaped host types, so resolution is ambiguous.
set -u
cd workdir || exit
"$KIO_BIN" check
