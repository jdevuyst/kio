#!/bin/sh
# Regression: a literal receives its carrier from an independently typed macro sibling.
set -u
cd workdir || exit
"$KIO_BIN" check
