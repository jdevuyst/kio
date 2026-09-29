#!/bin/sh
# A wide `|` chain that appears as a *call argument* (here an intrinsic
# form's leading type operand, `__left__(T, U, x)`) breaks to the
# leading-operator A1 layout — but, unlike a `type` body, a call-
# argument position is not self-delimiting, so the broken chain must
# parenthesize itself or its leading `|` reparses as a stray operator at
# the start of a type expression. Formatting must be idempotent and the
# output must round-trip (parse cleanly), which the second `fmt`
# asserts. Regression for the pretty-printer emitting an unparenthesized
# leading-operator chain at a bare type-operand position.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp workdir/main.kio "$work/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
"$KIO_BIN" fmt >/dev/null || exit
cat main.kio
