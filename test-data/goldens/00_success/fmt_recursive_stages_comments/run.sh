#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
mod main;
fn f() { recur // header
f[ // type
A] // after type
(a // first
, b) // between
( // second
c) }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p')
test "$comments" = '// header
// type
// after type
// first
// between
// second'
printf '%s\n' 'recursive stage comments preserved'
