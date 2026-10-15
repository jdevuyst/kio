#!/bin/sh
set -eu
cd workdir
"$KIO_BIN" build "$KIO_TARGET"
