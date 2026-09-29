#!/bin/sh
# `onto!` rejects sum widening — `A → A | B` adds a target branch
# the source can't reach, so the map isn't a surjection (no source
# value lands on the `B` arm). Widening is `into!`-only per spec
# § iso! / into! / onto! mechanics. Exit 15 (elaborator error).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
