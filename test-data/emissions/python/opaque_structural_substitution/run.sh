#!/bin/sh
# SUBJECT: Generic host unboxing supplies both fields of a public product to Kio consumers.
# CONTRACT: specs/backends/python.md, Host record contract and FFI surface.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-opaque-substitution.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

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
  if pyright --project pyrightconfig.json --outputjson consumer_index_negative.py \
      >index-negative.json 2>index-negative.stderr; then
    printf 'invariant Box unexpectedly accepted the wrong sum index\n' >&2
    exit 1
  fi
  python3 - index-negative.json <<'PY'
import json
import sys
from pathlib import Path

with open(sys.argv[1], encoding="utf-8") as stream:
    errors = [item for item in json.load(stream)["generalDiagnostics"]
              if item["severity"] == "error"]
if len(errors) != 1:
    raise SystemExit(f"unexpected invariant-index diagnostics: {errors}")
error = errors[0]
if (Path(error["file"]).name != "consumer_index_negative.py"
        or error["range"]["start"]["line"] != 16
        or error.get("rule") != "reportReturnType"
        or "bool" not in error["message"]
        or "str" not in error["message"]):
    raise SystemExit(f"unexpected invariant-index rejection: {error}")
PY
  PYTHONPATH=../workdir/out/python python3 consumer.py
)
