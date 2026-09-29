#!/bin/sh
# UFCS uses the same deferred E-App plan as the equivalent prefix call.
set -u
cd workdir || exit
"$KIO_BIN" check
