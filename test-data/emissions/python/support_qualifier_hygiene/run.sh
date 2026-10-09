#!/bin/sh
# SUBJECT: Ordinary host and export methods named typing do not capture annotations on sibling bottom-returning methods.
# CONTRACT: specs/backends/python.md, Typed stub and Item naming.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-qualifier.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

cd "$scratch/host"
if ! pyright --project pyrightconfig.json consumer.py >pyright.log 2>&1; then
  cat pyright.log >&2
  exit 1
fi
if pyright --project pyrightconfig.json --outputjson consumer_negative.py \
    >pyright-negative.json 2>pyright-negative.stderr; then
  printf 'unit-returning export unexpectedly satisfied a bottom-returning callback\n' >&2
  exit 1
fi
python3 - pyright-negative.json <<'PY'
import json
import sys
from pathlib import Path

with open(sys.argv[1], encoding="utf-8") as stream:
    errors = [item for item in json.load(stream)["generalDiagnostics"]
              if item["severity"] == "error"]
if (len(errors) != 1
        or Path(errors[0]["file"]).name != "consumer_negative.py"
        or errors[0].get("rule") != "reportArgumentType"
        or "None" not in errors[0]["message"]
        or "NoReturn" not in errors[0]["message"]):
    raise SystemExit(f"unexpected bottom-callback diagnostics: {errors}")
PY

export MYPYPATH=../workdir/out/python
if ! mypy --strict --python-version 3.10 --no-incremental \
    --cache-dir="$scratch/mypy-cache" --show-error-codes \
    consumer.py >mypy-positive.log 2>&1; then
  cat mypy-positive.log >&2
  exit 1
fi
if mypy --strict --python-version 3.10 --no-incremental \
    --cache-dir="$scratch/mypy-cache" --show-error-codes \
    consumer_negative.py >mypy-negative.log 2>&1; then
  printf 'unit-returning export unexpectedly satisfied a bottom-returning callback\n' >&2
  exit 1
fi
python3 - mypy-negative.log <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
errors = [line for line in text.splitlines() if ": error:" in line]
if (len(errors) != 1
        or re.fullmatch(r"consumer_negative\.py:11: error: .*\[arg-type\]", errors[0]) is None
        or "Callable[[], None]" not in errors[0]
        or "Callable[[], Never]" not in errors[0]):
    raise SystemExit(f"unexpected bottom-callback diagnostics:\n{text}")
PY

PYTHONPATH=../workdir/out/python python3 consumer.py
