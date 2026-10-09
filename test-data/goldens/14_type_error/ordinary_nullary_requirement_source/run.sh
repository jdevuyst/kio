#!/bin/sh
# Shared diagnostic phase only; no generated host artifact is consumed.
set -u
cd workdir || exit
"$KIO_BIN" check
