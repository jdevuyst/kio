#!/bin/sh
set -eu

# Grouped existing references preserve the selected function, including a
# finalizer's explicit absence. Distinct closed polymorphic values make each
# equivalence sensitive to choosing the wrong operand or function. Inputs
# retain grouping deliberately; this compiler-only case emits no host code.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
cp package.input "$work/semantic.pkg.kio"
cp main.input "$work/main.kio"
(cd "$work" && "$KIO_BIN" --no-cache test) >/dev/null
printf '%s\n' 'grouped references preserve meaning'
