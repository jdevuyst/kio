#!/bin/sh
# Pins that `kio test` scopes its equiv run to the consumer's own
# modules: the `app` package depends on `libdep` (which has its own
# `equiv lib_unit_law`), but a default `kio test` discharges only the
# consumer's `app_unit_law` (result 1/1, no `lib_unit_law` line). The
# skipped dependency block is reported on stderr — see expected.stderr.
# `--include-deps` opts the dependency module's equiv back in (result
# 2/2, both blocks). Per specs/cli.md § kio test.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" test || exit
"$KIO_BIN" test --include-deps
