#!/bin/sh
# Regression: a completed macro waits for its independently typed sibling result.
set -u
cd workdir || exit
"$KIO_BIN" check
