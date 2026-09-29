#!/bin/sh
# An unknown flag passed to a known subcommand is a CLI usage error
# (exit 2 per `specs/exit-codes.md`). Distinct from a build error
# (exit 40): nothing in the package is consulted — the flag is
# rejected at argument parsing, before any source is read.
set -u
"$KIO_BIN" build --bogusflag
