#!/bin/sh
# The block cannot determine a receiver factory's missing carrier argument.
set -u
cd workdir || exit
"$KIO_BIN" check
