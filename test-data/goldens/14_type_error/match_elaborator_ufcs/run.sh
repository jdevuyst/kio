#!/bin/sh
# Declared trailing blocks must be written as blocks; a blockless UFCS call
# supplying the projected clause product is a type error.
set -u
cd workdir || exit
"$KIO_BIN" check
