#!/bin/sh
# SUBJECT: A strict Python host uses an unsaturated constructor/application witness plus rank-N, existential, and recursive relationships.
# CONTRACT: specs/backends/python.md §§ FFI surface, Typed stub, Higher-kinded types
set -eu

if command -v mypy >/dev/null 2>&1; then
  MYPY=mypy
else
  printf 'rich_typed_host: required mypy is unavailable; install core mise tools\n' >&2
  exit 2
fi
case $("$MYPY" --version) in
  'mypy 2.4.0 '*) ;;
  *) printf 'rich_typed_host: required mypy version is 2.4.0\n' >&2; exit 2 ;;
esac

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
  if ! pyright --project pyrightconfig.json consumer.py consumer_callable.py >pyright.log 2>&1; then
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
  if pyright --project pyrightconfig.json --outputjson consumer_callable_negative.py \
      >callable-negative.json 2>callable-negative.stderr; then
    printf 'invalid callable assignments or calls were accepted\n' >&2
    exit 1
  fi
  python3 - callable-negative.json <<'PY'
import json
import sys
from pathlib import Path

with open(sys.argv[1], encoding="utf-8") as stream:
    errors = [item for item in json.load(stream)["generalDiagnostics"]
              if item["severity"] == "error"]
expected = {5: "reportReturnType", 9: "reportReturnType",
            13: "reportReturnType", 17: "reportReturnType",
            21: "reportCallIssue", 22: "reportCallIssue",
            23: "reportCallIssue", 24: "reportArgumentType",
            28: "reportReturnType", 32: "reportReturnType"}
actual = {item["range"]["start"]["line"]: item.get("rule") for item in errors}
if len(errors) != len(expected) or actual != expected or any(
        Path(item["file"]).name != "consumer_callable_negative.py" for item in errors):
    raise SystemExit(f"unexpected callable diagnostics: {errors}")
PY
  export MYPYPATH=../workdir/out/python
  if ! "$MYPY" --strict --python-version 3.10 --no-incremental \
      --cache-dir="$scratch/mypy-cache" --show-error-codes \
      consumer.py consumer_callable.py >mypy-positive.log 2>&1; then
    cat mypy-positive.log >&2
    exit 1
  fi
  for negative in consumer_stage_negative consumer_callable_negative; do
    if "$MYPY" --strict --python-version 3.10 --no-incremental \
        --cache-dir="$scratch/mypy-cache" --show-error-codes \
        "$negative.py" >"$negative.mypy.log" 2>&1; then
      printf 'mypy unexpectedly accepted %s\n' "$negative" >&2
      exit 1
    fi
    python3 - "$negative" "$negative.mypy.log" <<'PY'
import re
import sys
from pathlib import Path

name, logfile = sys.argv[1:]
expected = ([(6, "arg-type")] if name == "consumer_stage_negative" else
            [(6, "return-value"), (10, "return-value"),
             (14, "return-value"), (18, "return-value"),
             (22, "call-arg"), (23, "call-arg"), (24, "call-arg"),
             (25, "arg-type"), (29, "return-value"), (33, "return-value")])
text = Path(logfile).read_text(encoding="utf-8")
errors = [line for line in text.splitlines() if ": error:" in line]
actual = []
for error in errors:
    match = re.fullmatch(r"(.+\.py):(\d+): error: .*\[([\w-]+)\]", error)
    if match is None or Path(match[1]).name != name + ".py":
        raise SystemExit(f"unexpected mypy diagnostic:\n{text}")
    actual.append((int(match[2]), match[3]))
if sorted(actual) != sorted(expected):
    raise SystemExit(f"unexpected mypy negative diagnostics:\n{text}")
PY
  done
  PYTHONPATH=../workdir/out/python python3 consumer.py
)
