#!/bin/sh
# A parametric equiv that fails: two same-typed value parameters
# bind to distinct fresh atoms, so `term x` and `term y` are
# unequal NFs. Confirms the parametric runner reports per-group
# breakdowns just like the non-parametric path, and pins the
# `__param_<name>__` atom-naming scheme.
set -u
cd workdir || exit
"$KIO_BIN" test
