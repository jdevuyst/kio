#!/bin/sh
# A type-parameter that appears in NEITHER the value-arg types
# NOR the callee's return type cannot be inferred — not from
# value-arg unification, not from the call's expected return type.
# language.md § Type system: inference is localized and structural,
# so a binder reachable from neither path is a type error (exit
# code 14, "cannot infer type argument").
set -u
cd workdir || exit
"$KIO_BIN" check
