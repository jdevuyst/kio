#!/bin/sh
# Contextual lambda alignment preserves a nested quantified parameter's name.
set -u
cd workdir || exit
"$KIO_BIN" check
