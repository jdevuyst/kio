#!/bin/sh
# Variance-aware strict-positivity: a `rec` reference smuggled into
# a negative position via a parametric type application is still
# rejected. Without per-parameter variance the bootstrap would
# accept this and break strong normalization.
set -u
cd workdir || exit
"$KIO_BIN" check
