#!/bin/sh
# A nested partial call draws each placeholder from its checked result slot.
set -u
cd workdir || exit
"$KIO_BIN" check
