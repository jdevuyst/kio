#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
mod fmt_type_path_bracket_comments;
type Single = Box // between head and bracket
[// first argument
A // argument trailing
] // between stages
[B];
type Qualified = pkg // qualified path
/module.Type[// qualified type arg
A] // between qualified stages
[B];
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p')
test "$comments" = '// between head and bracket
// first argument
// argument trailing
// between stages
// qualified path
// qualified type arg
// between qualified stages'
printf '%s\n' 'type path bracket comments retained'
