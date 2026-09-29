#!/bin/sh
# A wide candidate tuple where one listed candidate has a kind-invalid
# result type. This keeps the kind diagnostic stable while surrounding
# the bad candidate with irrelevant well-kinded rules.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" --no-cache check
