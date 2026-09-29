#!/bin/sh
# `rec newtype` is rejected when the payload does not use its own head.
set -u
cd workdir || exit
"$KIO_BIN" check
