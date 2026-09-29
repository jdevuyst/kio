#!/bin/sh
# Pins canonical `kio fmt` output for a single .kio file that
# exercises every module-level construct: every `import` shape, every
# top-level item kind (fn / type / literal / newtype, with and without
# `pub`, with and without type parameters), every type form (path,
# unit, bottom, sum, product, function, rec), and every value form
# (call, let with and without annotation, fn with and without return,
# dotted-path callee, mixed type/value args, every literal kind).
#
# Strategy: workdir/fmt_canonical/main.kio is the deliberately-minified single-line
# input — kept tracked in that form so a regression that mis-formats
# is visible in expected.stdout, not in workdir/. The test copies it to a
# matching path in a scratch directory so `kio fmt` (which rewrites in place)
# doesn't canonicalize the tracked source or break the declared module suffix.
# Then it (1) runs `kio fmt` and
# prints the canonical form via `cat`, and (2) re-runs `kio fmt` and
# prints again. The two cat outputs in expected.stdout must match —
# first cat proves fmt actually formats, second cat proves fmt is a
# fixed point of itself.
#
# fmt's progress output (which file it rewrote) is sent to /dev/null
# so the test only diffs canonical content.
set -u
work=$(mktemp -d) || exit
trap 'rm -rf "$work"' EXIT INT TERM HUP
mkdir -p "$work/fmt_canonical" || exit
cp workdir/fmt_canonical/main.kio "$work/fmt_canonical/main.kio" || exit
cd "$work" || exit
"$KIO_BIN" fmt fmt_canonical/main.kio >/dev/null || exit
cat fmt_canonical/main.kio
"$KIO_BIN" fmt fmt_canonical/main.kio >/dev/null || exit
cat fmt_canonical/main.kio
