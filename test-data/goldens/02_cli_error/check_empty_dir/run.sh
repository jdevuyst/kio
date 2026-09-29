#!/bin/sh
# `kio check` with no positional selector in a directory containing zero
# `.kio` files is a CLI usage error (exit 2 per `specs/exit-codes.md`):
# there is nothing to check. Distinct from a directory that has `.kio`
# source but checks clean. An explicit empty scratch dir keeps the case
# self-contained regardless of the harness cwd.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cd "$work" || exit
"$KIO_BIN" check
