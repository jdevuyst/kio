#!/bin/sh
# A consumer-visible literal alias has one source identity. Import order must
# never choose between two distinct exported aliases with the same name.
set -u
cd workdir || exit
"$KIO_BIN" check
