#!/bin/sh
# An elaborator with declared trailing blocks requires its direct block call;
# supplying the projected clause product through UFCS is a type error.
set -u
cd workdir || exit
"$KIO_BIN" check
