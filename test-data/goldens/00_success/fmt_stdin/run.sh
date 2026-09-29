#!/bin/sh
# `kio fmt -` (stdin mode) per `specs/cli.md` § kio fmt: read
# source from stdin, write canonical form to stdout, parsing as a
# regular module. The input here is non-canonical (squeezed
# spaces, a missing trailing newline); the output is the
# canonical form (idempotent under a second `kio fmt -`).
set -u
input='module x;fn id[A](x:A)->A{x}'
out=$(printf '%s' "$input" | "$KIO_BIN" fmt -)
status=$?
[ "$status" = 0 ] || { printf 'kio fmt - exited %s\n' "$status" >&2; exit 1; }
printf '%s\n' "$out"
# Idempotence: re-formatting the formatter's output should be a
# no-op (byte-identical, including the trailing newline the
# formatter always emits).
out2=$(printf '%s\n' "$out" | "$KIO_BIN" fmt -)
[ "$out" = "$out2" ] || { printf 'fmt - is not idempotent\n' >&2; exit 1; }
