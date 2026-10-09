#!/bin/sh
# SUBJECT: A concrete callable view keeps exact nominal validation through opaque generic storage.
# CONTRACT: specs/backends/python.md, FFI surface.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-specialized-callable.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
cd "$scratch/workdir"

for package in . peer; do
  (
    cd "$package"
    "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  )
done

(
  cd "$scratch/host"
  if ! pyright --project pyrightconfig.json consumer.py >pyright.log 2>&1; then
    cat pyright.log >&2
    exit 1
  fi
  PYTHONPATH=../workdir/out/python:../workdir/peer/out/python python3 consumer.py
)
