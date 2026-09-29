#!/bin/sh
# Pins that `kio test` reduces surface forms whose meaning the
# typer elaborates into Kio' (`if`/`else`, `match!`) — the
# evaluator reduces the elaborated form, not the surface form
# treated as an opaque atom. Two structurally identical
# `if`/`else` (resp. `match!`) terms therefore residualize to
# the same NF group per `specs/cli.md` § kio test.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" test
