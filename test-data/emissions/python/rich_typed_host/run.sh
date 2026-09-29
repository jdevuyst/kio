#!/bin/sh
# SUBJECT: A strict Python host uses an unsaturated constructor/application witness plus rank-N, existential, and recursive relationships.
# CONTRACT: specs/backends/python.md §§ FFI surface, Typed stub, Higher-kinded types
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-rich-host.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

(
  cd "$scratch/host"
  if ! pyright --project pyrightconfig.json consumer.py >pyright.log 2>&1; then
    cat pyright.log >&2
    exit 1
  fi
  PYTHONPATH=../workdir/out/python python3 consumer.py
)
