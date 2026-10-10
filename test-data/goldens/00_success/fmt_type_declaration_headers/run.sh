#!/bin/sh
set -eu

"$KIO_BIN" fmt --check workdir/app.sig.kio

formatted=$("$KIO_BIN" fmt - <<'KIO'
module fmt_type_declaration_headers;
host type // host name
String // host role
role(// role start
str // role end
) // host terminator
;
newtype // newtype name
Box[A] // newtype colon
: // payload start
A // member opener
{ pub // constructor keyword
constructor // constructor name
mk; pub // projector keyword
projector // projector name
get }
KIO
)
reformatted=$(printf '%s\n' "$formatted" | "$KIO_BIN" fmt -)
test "$formatted" = "$reformatted"
comments=$(printf '%s\n' "$formatted" | sed -n 's/.*\/\/ */\/\/ /p')
test "$comments" = '// host name
// host role
// role start
// role end
// host terminator
// newtype name
// newtype colon
// payload start
// member opener
// constructor keyword
// constructor name
// projector keyword
// projector name'
printf '%s\n' 'type declaration comments retained'
