#!/bin/sh
# A bare imported name has one consumer-written source identity. Importing the
# same spelling from two modules is a name error, never map-order selection.
set -u
cd workdir || exit
"$KIO_BIN" check
