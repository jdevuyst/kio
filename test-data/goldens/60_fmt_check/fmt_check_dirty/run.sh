#!/bin/sh
# `kio fmt --check` on a non-canonical file lists the dirty path on
# stdout and exits 60 (per specs/exit-codes.md § 6x fmt-check tier;
# differs from gofmt -l / cargo fmt --check, both of which use 1).
# Pairs with `fmt_check_clean` (which exits 0 with empty stdout).
# The case's source on disk is deliberately squeezed (no spaces
# around `,` / `:` / `.>` / `{}`), so the formatter would rewrite
# it; --check is what reports that without writing.
set -u
cd workdir || exit
out=$("$KIO_BIN" fmt --check main.kio)
status=$?
# Pin the dirty-path line on stdout (it's the case-relative path, and
# stable across implementations). The exit status is the load-bearing
# part — we forward it as the final exit so the runner's
# expected.exit assertion catches it.
printf '%s\n' "$out"
exit "$status"
