#!/bin/sh
set -eu

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_grouped_type_comments;
fn ordinary() -> (// ordinary result
.) { () }
host fn supplied() -> (. // host result
);
type Alias = (// alias start
(// nested start
. // nested end
) // alias end
);
type Function = (// function domain
.) -> (// function result
.);
type Applied = Pair((. // first argument
), (. // last argument
));
type Chained = ((// chain start
. // chain end
) & .);
type Adjacent = (A // adjacent separator
&.);
type Bare_product = & // product leading
& A // product trailing
&;
type Bare_sum = | // sum leading
| A // sum trailing
|;
type Empty_product = & // empty product
&;
type Empty_sum = | // empty sum
|;
type Final_product = A & // final product
;
type Final_sum = | // final sum
;
type // alias keyword
Header[// alias binder
A // alias binder end
] // alias equals
= // alias body
A // alias terminator
;
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p')
test "$comments" = '// ordinary result
// host result
// alias start
// nested start
// nested end
// alias end
// function domain
// function result
// first argument
// last argument
// chain start
// chain end
// adjacent separator
// product leading
// product trailing
// sum leading
// sum trailing
// empty product
// empty sum
// final product
// final sum
// alias keyword
// alias binder
// alias binder end
// alias equals
// alias body
// alias terminator'
printf '%s\n' 'grouped type comments retained'
