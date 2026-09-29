#!/bin/sh
# `let x <- e;` is admissible only inside a do-block. Outside
# any do-block, the standard let-statement parser expects `=`
# after the bound name; encountering `<-` is a parse error.
set -u
cd workdir || exit
"$KIO_BIN" check
