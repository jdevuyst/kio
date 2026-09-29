#!/bin/sh
# SUBJECT: The Python runtime module and typed stub expose exact branded products and three-arm sums to strict hosts.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python.XXXXXX")
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

  if pyright --project pyrightconfig.json consumer_negative.py >pyright-negative.log 2>&1; then
    printf 'consumer_negative.py unexpectedly accepted an omitted Choice arm\n' >&2
    exit 1
  fi
  negative_error_count=$(grep -c ' - error: ' pyright-negative.log || true)
  if [ "$negative_error_count" -ne 1 ] ||
     ! grep -Eq 'consumer_negative\.py:58:[0-9]+ - error:' pyright-negative.log ||
     ! grep -qF 'NoReturn' pyright-negative.log ||
     ! grep -qF 'assert_never' pyright-negative.log; then
    printf 'consumer_negative.py failed for an unexpected reason\n' >&2
    sed 's/^/    /' pyright-negative.log >&2
    exit 1
  fi

  if pyright --project pyrightconfig.json --outputjson consumer_ingress_negative.py \
      >pyright-ingress.json 2>pyright-ingress.stderr; then
    printf 'consumer_ingress_negative.py unexpectedly accepted malformed inputs\n' >&2
    exit 1
  fi
  if ! python3 - pyright-ingress.json <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    report = json.load(stream)

errors = [
    diagnostic
    for diagnostic in report["generalDiagnostics"]
    if diagnostic["severity"] == "error"
]
expected = {
    37: ("Count", "required"),
    38: ("str", "int"),
    39: ("dict[str, int]", "echoChoice"),
    40: ("dict[str, int]", "echoChoice"),
}
lines = [diagnostic["range"]["start"]["line"] + 1 for diagnostic in errors]
if len(errors) != len(expected):
    raise SystemExit(f"unexpected ingress diagnostic count: {len(errors)}")
if len(set(lines)) != len(lines):
    raise SystemExit(f"duplicate ingress diagnostic lines: {lines}")
actual = {line: diagnostic["message"] for line, diagnostic in zip(lines, errors)}
if set(actual) != set(expected):
    raise SystemExit(f"unexpected ingress diagnostic lines: {sorted(actual)}")
for line, fragments in expected.items():
    message = actual[line]
    if any(fragment not in message for fragment in fragments):
        raise SystemExit(f"unexpected ingress diagnostic at line {line}: {message}")
PY
  then
    printf 'consumer_ingress_negative.py failed for unexpected reasons\n' >&2
    sed 's/^/    /' pyright-ingress.json >&2
    sed 's/^/    /' pyright-ingress.stderr >&2
    exit 1
  fi

  PYTHONPATH=../workdir/out/python python3 runtime_consumer.py
)
