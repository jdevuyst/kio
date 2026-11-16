#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
mod fmt_qualified_value_path_comments;
fn callee() { m // before dot
. // before segment
ready // before call
() }
fn argument() { call(m // argument before dot
. // argument before segment
ready // before close
) }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/^[[:space:]]*\(\/\/.*\)$/\1/p')
test "$comments" = '// before dot
// before segment
// before call
// argument before dot
// argument before segment
// before close'
printf '%s\n' 'qualified value path comments retained'
