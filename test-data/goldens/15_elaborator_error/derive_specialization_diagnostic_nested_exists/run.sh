#!/bin/sh
# After one derivation succeeds, a fatal specialization diagnostic in a later
# rule's recursive subgoal reaches the existence-only ambiguity-counting path.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
