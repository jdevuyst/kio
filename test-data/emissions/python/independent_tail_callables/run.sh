#!/bin/sh
# SUBJECT: Strict Python host code exchanges independently generated generic stages and nested cooperative results.
# CONTRACT: specs/backends/python.md § Cooperative callable protocol
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-independent-tail.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  cd peer
  "$KIO_BIN" build "$KIO_TARGET"
)
(
  cd "$scratch/host"
  PYTHONPATH=$(python3 -c 'import os; print(os.pathsep.join(("../workdir/out/python", "../workdir/peer/out/python")))')
  export PYTHONPATH
  export MYPYPATH="$PYTHONPATH"
  if ! pyright --project pyrightconfig.json consumer.py >pyright.log 2>&1; then
    cat pyright.log >&2
    exit 1
  fi
  if ! mypy --strict --python-version 3.10 --no-incremental \
      --cache-dir="$scratch/mypy-cache" consumer.py >mypy.log 2>&1; then
    cat mypy.log >&2
    exit 1
  fi
  python3 consumer.py
)
