#!/bin/sh
# A qualified type path must name a type declared by the imported module.
set -u
cd workdir || exit
"$KIO_BIN" check
