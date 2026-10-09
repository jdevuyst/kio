#!/bin/sh
# SUBJECT: Independent generated Python packages share complete static stage definitions without sharing runtime ownership.
# CONTRACT: specs/backends/python.md, Typed stub and Readable type views.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-shared-stages.XXXXXX")
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

cd "$scratch/host"
for order in ab ba; do
  if ! pyright --project "$order.json" consumer.py >"$order.log" 2>&1; then
    cat "$order.log" >&2
    exit 1
  fi
  if pyright --project "$order.json" --outputjson negative.py >"negative-$order.json" 2>"negative-$order.stderr"; then
    printf 'invalid stage selections were accepted\n' >&2
    exit 1
  fi
  python3 - "negative-$order.json" <<'PY'
import json
import sys
from pathlib import Path

with open(sys.argv[1], encoding="utf-8") as stream:
    errors = [item for item in json.load(stream)["generalDiagnostics"]
              if item["severity"] == "error"]
expected = {6: "reportArgumentType", 12: "reportArgumentType",
            16: "reportReturnType", 20: "reportReturnType",
            24: "reportReturnType", 28: "reportReturnType",
            38: "reportArgumentType"}
actual = {item["range"]["start"]["line"]: item.get("rule") for item in errors}
if len(errors) != len(expected) or actual != expected or any(
        Path(item["file"]).name != "negative.py" for item in errors):
    raise SystemExit(f"unexpected exact-stage diagnostics: {errors}")
PY
done
for package in first second; do
  if ! pyright --project "$package.json" "${package}_only.py" >"$package.log" 2>&1; then
    cat "$package.log" >&2
    exit 1
  fi
done
PYTHONPATH=../workdir/out/python:../workdir/peer/out/python python3 consumer.py
PYTHONPATH=../workdir/peer/out/python:../workdir/out/python python3 consumer.py
PYTHONPATH=../workdir/out/python python3 dynamic.py
