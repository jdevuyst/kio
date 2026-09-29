#!/bin/sh
# Transparent aliases cannot form a recursive component by themselves.
set -u
cd workdir || exit
"$KIO_BIN" check
