#!/bin/sh
# `kio fmt --check` reading from stdin (the `-` form) on
# non-canonical input lists the `<stdin>` marker on stdout and
# exits 60 (per specs/exit-codes.md § 6x fmt-check tier;
# specs/cli.md § kio fmt). This pins the stdin `--check` path to
# the same exit code as the file-path path (60_fmt_check/
# fmt_check_dirty), not the internal-error category (1): a
# formatting difference is a user-input outcome, not a compiler bug.
# The piped source is deliberately squeezed (no spaces around `,` /
# `:`, no trailing newline), so the formatter would rewrite it;
# --check reports that without writing.
set -u
input='module x;fn id[A](x:A)->A{x}'
out=$(printf '%s' "$input" | "$KIO_BIN" fmt --check -)
status=$?
# Pin the dirty marker line on stdout (`<stdin>`, stable across
# implementations). The exit status is the load-bearing part — we
# forward it as the final exit so the runner's expected.exit
# assertion catches it.
printf '%s\n' "$out"
exit "$status"
