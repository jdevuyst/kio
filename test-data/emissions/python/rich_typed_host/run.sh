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
  if pyright --project pyrightconfig.json --outputjson consumer_stage_negative.py \
      >stage-negative.json 2>stage-negative.stderr; then
    printf 'the selected int stage unexpectedly accepted str\n' >&2
    exit 1
  fi
  python3 - stage-negative.json <<'PY'
import json
import sys
from pathlib import Path

with open(sys.argv[1], encoding="utf-8") as stream:
    errors = [item for item in json.load(stream)["generalDiagnostics"]
              if item["severity"] == "error"]
if len(errors) != 1:
    raise SystemExit(f"unexpected selected-stage diagnostics: {errors}")
error = errors[0]
if (Path(error["file"]).name != "consumer_stage_negative.py"
        or error["range"]["start"]["line"] != 5
        or error.get("rule") != "reportArgumentType"
        or "int" not in error["message"]
        or "Literal['wrong']" not in error["message"]):
    raise SystemExit(f"unexpected selected-stage rejection: {error}")
PY
  PYTHONPATH=../workdir/out/python python3 consumer.py
)
