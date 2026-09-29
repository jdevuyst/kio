#!/bin/sh
# SUBJECT: Wide-label Go emission retains the measured call-once-IIFE and generated-line bounds.
# CONTRACT: Recorded statement-lowering measurement in aa3f0d9fde2a87b5d2c651ea2e99aa97ecc4938c.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-wide-label.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

source_file=out/go/pkg.go
test -f "$source_file"

iife_count=$(awk '
  {
    rest = $0
    while ((at = index(rest, "func() any {")) != 0) {
      count++
      rest = substr(rest, at + length("func() any {"))
    }
  }
  END { print count + 0 }
' "$source_file")
if [ "$iife_count" -gt 2 ]; then
  printf 'wide-label Go body has %s call-once IIFEs; measured bound is 2\n' "$iife_count" >&2
  exit 1
fi

max_line=$(awk '{ if (length($0) > max) max = length($0) } END { print max + 0 }' "$source_file")
if [ "$max_line" -gt 1000 ]; then
  printf 'wide-label Go body has a %s-character line; measured bound is 1000\n' "$max_line" >&2
  exit 1
fi
