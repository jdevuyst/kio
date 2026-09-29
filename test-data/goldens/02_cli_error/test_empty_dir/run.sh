#!/bin/sh
# `kio test` with no positional selector in a directory containing zero
# `.kio` files is a CLI usage error (exit 2 per `specs/exit-codes.md`):
# there is nothing to test. This is distinct from a directory that has
# `.kio` source but declares no `equiv` blocks — that case prints
# "no equiv blocks found" and exits 0. An explicit empty scratch dir
# keeps the case self-contained regardless of the harness cwd.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
cd "$work" || exit
"$KIO_BIN" test
