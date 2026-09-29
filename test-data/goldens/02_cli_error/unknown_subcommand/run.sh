#!/bin/sh
# An unknown top-level subcommand is a CLI usage error (exit 2 per
# `specs/exit-codes.md`). The case has no `workdir/` — the error fires
# before any package is read, so there's nothing to put on disk.
set -u
"$KIO_BIN" nonsensesubcommand
