#!/bin/sh
# Term declarations must use lowercase- or underscore-initial names.
# This pins the parser-level convention at a direct function
# definition site.
set -u
cd workdir || exit
"$KIO_BIN" check
