#!/bin/sh
# A zero-argument call feeds the implicit unit packet to the first
# remaining value group even when the callee is a computed closure.
# The passing arm pins substitution into the following positive group;
# the failing arm pins that two distinct positive-group arguments stay
# distinct after consecutive zero-slot groups.
set -u
cd workdir || exit
"$KIO_BIN" test
