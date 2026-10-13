#!/bin/sh
# SUBJECT: A callback's mutable product keeps the generated host binding invariant under strict Python checking.
# CONTRACT: specs/backends/python.md §§ FFI surface, Typed stub
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-protocol-variance.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

(
  cd "$scratch/host"
  if ! pyright --project pyrightconfig.json consumer.py ../workdir/out/python/protocol_variance >pyright.log 2>&1; then
    cat pyright.log >&2
    exit 1
  fi
  export MYPYPATH=../workdir/out/python
  if ! mypy --strict --python-version 3.10 --no-incremental \
      --cache-dir="$scratch/mypy-cache" consumer.py >mypy.log 2>&1; then
    cat mypy.log >&2
    exit 1
  fi

  if pyright --project pyrightconfig.json --outputjson consumer_negative.py >negative.json 2>negative.stderr; then
    printf 'Pyright accepted incompatible host bindings\n' >&2
    exit 1
  fi
  if mypy --strict --python-version 3.10 --no-incremental \
      --cache-dir="$scratch/mypy-cache" --show-error-codes consumer_negative.py >negative.mypy 2>&1; then
    printf 'mypy accepted incompatible host bindings\n' >&2
    exit 1
  fi
  python3 - <<'PY'
import json
import re
from pathlib import Path

errors = [item for item in json.loads(Path("negative.json").read_text())["generalDiagnostics"]
          if item["severity"] == "error"]
actual = [(Path(item["file"]).name, item["range"]["start"]["line"] + 1, item.get("rule"))
          for item in errors]
expected = [("consumer_negative.py", line, "reportReturnType") for line in [9, 13]]
if sorted(actual) != expected:
    raise SystemExit(f"unexpected Pyright diagnostics: {errors}")
text = Path("negative.mypy").read_text()
actual = re.findall(r"^consumer_negative\.py:(\d+): error: .*\[return-value\]$", text, re.M)
if actual != ["9", "13"] or text.count(": error:") != 2:
    raise SystemExit(f"unexpected mypy diagnostics: {text}")
PY
  PYTHONPATH=../workdir/out/python python3 consumer.py
)
