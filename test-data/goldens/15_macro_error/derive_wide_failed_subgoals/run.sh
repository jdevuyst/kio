#!/bin/sh
# A wide derive tuple whose candidates all match the requested result
# head, then fail through distinct subgoals. This protects the resolver
# path where failed subgoal resolution, not initial candidate filtering,
# decides that no rule applies.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
