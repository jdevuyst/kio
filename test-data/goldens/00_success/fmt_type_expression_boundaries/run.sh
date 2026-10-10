#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_type_expression_boundaries;
type Universal = [// binder start
A // binder end
] // universal body
A;
type Arrow = (A // arrow before
-> // arrow after
B);
type Applied = Box // application opener
(A);
type Empty = Box(// empty application
);
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p')
test "$comments" = '// binder start
// binder end
// universal body
// arrow before
// arrow after
// application opener
// empty application'
printf '%s\n' 'type expression comments retained'
