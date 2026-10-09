#!/bin/sh
# Self-test for the Python stub gate's per-runtime completeness check.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
CHECK=$REPO_ROOT/ci/checks/per-case/pyright-strict.sh
if command -v mise >/dev/null 2>&1 && SELFTEST_NODE=$(cd "$REPO_ROOT" && mise which node 2>/dev/null) && [ -n "$SELFTEST_NODE" ]; then
  :
else
  SELFTEST_NODE=$(command -v node)
fi
export SELFTEST_NODE
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
fixture=$(mktemp -d "$scratch_parent/pyright-strict-selftest.XXXXXX")
trap 'rm -rf "$fixture"' EXIT INT TERM HUP

mkdir -p "$fixture/bin" "$fixture/case/workdir"

cat >"$fixture/bin/kio" <<'SH'
#!/bin/sh
set -eu
if [ "$*" = 'debug build-output-plan python' ]; then
  printf '[{"package_root":"%s","target":"python","backend":"python","output_dir":"%s/out/python","namespace":"alpha"}' "$PWD" "$PWD"
  if [ "${KIO_FAKE_STUB_SCENARIO:-missing-beta}" = missing-beta ]; then
    printf ',{"package_root":"%s","target":"python","backend":"python","output_dir":"%s/out/python","namespace":"beta"}' "$PWD" "$PWD"
  fi
  printf ']\n'
  exit 0
fi
[ "$1" = build ]
[ "$2" = python ]
case ${KIO_FAKE_STUB_SCENARIO:-missing-beta} in
  missing-beta)
    mkdir -p out/python/alpha
    printf 'runtime = 1\n' >out/python/alpha.py
    printf 'runtime = 2\n' >out/python/beta.py
    printf 'from ._kio_stub_0000 import *\n' >out/python/alpha/__init__.pyi
    printf 'runtime: int\n' >out/python/alpha/_kio_stub_0000.pyi
    ;;
  missing-shard)
    mkdir -p out/python/alpha
    printf 'runtime = 1\n' >out/python/alpha.py
    printf 'from ._kio_stub_0000 import *\n' >out/python/alpha/__init__.pyi
    ;;
  missing-reexport)
    mkdir -p out/python/alpha
    printf 'runtime = 1\n' >out/python/alpha.py
    : >out/python/alpha/__init__.pyi
    printf 'runtime: int\n' >out/python/alpha/_kio_stub_0000.pyi
    ;;
  *) exit 2 ;;
esac
SH
chmod +x "$fixture/bin/kio"

cat >"$fixture/bin/pyright" <<'SH'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$PYRIGHT_CALL_LOG"
SH
chmod +x "$fixture/bin/pyright"

cat >"$fixture/bin/mise" <<'SH'
#!/bin/sh
set -eu
[ "$1" = which ]
[ "$2" != node ] || { printf '%s\n' "$SELFTEST_NODE"; exit 0; }
[ "$2" = pyright ]
printf '%s\n' "$FAKE_PYRIGHT"
SH
chmod +x "$fixture/bin/mise"

stderr=$fixture/stderr
calls=$fixture/pyright-calls
if (
  cd "$fixture/case"
  PATH="$fixture/bin:$PATH" \
    KIO_TARGET=python \
    KIO_BIN="$fixture/bin/kio" \
    FAKE_PYRIGHT="$fixture/bin/pyright" \
    PYRIGHT_CALL_LOG="$calls" \
    sh "$CHECK"
) 2>"$stderr"; then
  printf 'pyright-strict-selftest: missing beta stub package passed\n' >&2
  exit 1
fi

if ! grep -Fq 'out/python/beta.py` has no generated stub package' "$stderr"; then
  printf 'pyright-strict-selftest: missing per-runtime diagnostic\n' >&2
  cat "$stderr" >&2
  exit 1
fi

if [ "$(wc -l <"$calls")" -ne 1 ] || ! grep -Fq 'out/python/alpha' "$calls"; then
  printf 'pyright-strict-selftest: complete alpha stub package was not checked exactly once\n' >&2
  cat "$calls" >&2
  exit 1
fi

missing_shard_stderr=$fixture/missing-shard-stderr
if (
  cd "$fixture/case"
  PATH="$fixture/bin:$PATH" \
    KIO_TARGET=python \
    KIO_BIN="$fixture/bin/kio" \
    KIO_FAKE_STUB_SCENARIO=missing-shard \
    FAKE_PYRIGHT="$fixture/bin/pyright" \
    PYRIGHT_CALL_LOG="$fixture/missing-shard-calls" \
    sh "$CHECK"
) 2>"$missing_shard_stderr"; then
  printf 'pyright-strict-selftest: missing declaration shard passed\n' >&2
  exit 1
fi
if ! grep -Fq 'out/python/alpha` has no generated declaration shard' \
    "$missing_shard_stderr"; then
  printf 'pyright-strict-selftest: missing declaration-shard diagnostic\n' >&2
  cat "$missing_shard_stderr" >&2
  exit 1
fi

missing_reexport_stderr=$fixture/missing-reexport-stderr
if (
  cd "$fixture/case"
  PATH="$fixture/bin:$PATH" \
    KIO_TARGET=python \
    KIO_BIN="$fixture/bin/kio" \
    KIO_FAKE_STUB_SCENARIO=missing-reexport \
    FAKE_PYRIGHT="$fixture/bin/pyright" \
    PYRIGHT_CALL_LOG="$fixture/missing-reexport-calls" \
    sh "$CHECK"
) 2>"$missing_reexport_stderr"; then
  printf 'pyright-strict-selftest: missing shard re-export passed\n' >&2
  exit 1
fi
# shellcheck disable=SC2016 # backticks are literal diagnostic text
if ! grep -Fq 'does not re-export `_kio_stub_0000.pyi`' \
    "$missing_reexport_stderr"; then
  printf 'pyright-strict-selftest: missing shard re-export diagnostic\n' >&2
  cat "$missing_reexport_stderr" >&2
  exit 1
fi

# Exercise the real checker over the generated layout's hard cases. The first
# shard has exactly the production declaration bound; the second crosses that
# boundary and closes a nominal + TypeVar cycle back to the first. Public
# access goes only through wildcard re-exports from `__init__.pyi`. Separate
# 800-member Protocol and TypedDict declarations pin that Pyright's 768-module
# ceiling does not require splitting one declaration's member body.
if command -v mise >/dev/null 2>&1 &&
   PYRIGHT=$(cd "$REPO_ROOT" && mise which pyright 2>/dev/null) &&
   [ -n "$PYRIGHT" ]; then
  :
elif command -v pyright >/dev/null 2>&1; then
  PYRIGHT=pyright
else
  printf 'pyright-strict-selftest: pyright is required\n' >&2
  exit 2
fi

layout=$fixture/layout
mkdir -p "$layout/alpha" "$layout/beta"
cat >"$layout/alpha.py" <<'PY'
RUNTIME_MARKER = "alpha-runtime"

def make() -> str:
    return "alpha-runtime-value"
PY
cat >"$layout/beta.py" <<'PY'
RUNTIME_MARKER = "beta-runtime"

def make() -> str:
    return "beta-runtime-value"
PY
cat >"$layout/alpha/__init__.pyi" <<'PYI'
from ._kio_stub_0000 import *
from ._kio_stub_0001 import *
PYI
{
  printf 'from __future__ import annotations\nimport typing\n'
  printf 'from . import _kio_stub_0001 as _kio_stub_0001  # pyright: ignore[reportPrivateUsage]\n\n'
  index=0
  while [ "$index" -lt 254 ]; do
    printf "Filler%03d = typing.TypeVar('Filler%03d')\n" "$index" "$index"
    index=$((index + 1))
  done
  cat <<'PYI'
class Left(typing.TypedDict, typing.Generic[_kio_stub_0001.RightVar], total=False):
    value: _kio_stub_0001.RightVar
    peer: _kio_stub_0001.Right[_kio_stub_0001.RightVar]

_PrivateLeft: typing.TypeAlias = Left
PYI
} >"$layout/alpha/_kio_stub_0000.pyi"
cat >"$layout/alpha/_kio_stub_0001.pyi" <<'PYI'
from __future__ import annotations
import typing
from . import _kio_stub_0000 as _kio_stub_0000  # pyright: ignore[reportPrivateUsage]

RightVar = typing.TypeVar('RightVar')
PrivateBridge: typing.TypeAlias = _kio_stub_0000._PrivateLeft  # pyright: ignore[reportPrivateUsage]

class Right(typing.TypedDict, typing.Generic[RightVar], total=False):
    peer: _kio_stub_0000.Left[RightVar]

def make() -> _kio_stub_0000.Left[int]: ...
PYI
cat >"$layout/beta/__init__.pyi" <<'PYI'
from ._kio_stub_0000 import *
PYI
cat >"$layout/beta/_kio_stub_0000.pyi" <<'PYI'
import typing

class Marker(typing.TypedDict):
    value: int

def make() -> Marker: ...
PYI
cat >"$layout/consumer.py" <<'PY'
import alpha
import beta

left: alpha.Left[int] = alpha.make()
bridge: alpha.PrivateBridge = left
right: alpha.Right[int] = {"peer": left}
left["peer"] = right
marker: beta.Marker = beta.make()
PY
cat >"$layout/consumer_negative.py" <<'PY'
import alpha

wrong: alpha.Left[str] = alpha.make()
PY
cat >"$layout/runtime_probe.py" <<'PY'
import alpha
import beta

assert alpha.RUNTIME_MARKER == "alpha-runtime"
assert beta.RUNTIME_MARKER == "beta-runtime"
assert alpha.make() == "alpha-runtime-value"
assert beta.make() == "beta-runtime-value"
PY
cat >"$layout/pyrightconfig.json" <<'JSON'
{ "typeCheckingMode": "strict", "pythonVersion": "3.10" }
JSON
cat >"$layout/huge_protocol.pyi" <<'PYI'
import typing

class HugeProtocol(typing.Protocol):
PYI
index=0
while [ "$index" -lt 800 ]; do
  printf '    def method_%04d(self) -> int: ...\n' "$index" \
    >>"$layout/huge_protocol.pyi"
  index=$((index + 1))
done
cat >"$layout/huge_typed_dict.pyi" <<'PYI'
import typing

class HugeTypedDict(typing.TypedDict):
PYI
index=0
while [ "$index" -lt 800 ]; do
  printf '    field_%04d: int\n' "$index" >>"$layout/huge_typed_dict.pyi"
  index=$((index + 1))
done

alpha_first_declarations=$(awk '
  /^[^[:space:]#]/ && $1 != "from" && $1 != "import" { count++ }
  END { print count + 0 }
' "$layout/alpha/_kio_stub_0000.pyi")
alpha_all_declarations=$(awk '
  /^[^[:space:]#]/ && $1 != "from" && $1 != "import" { count++ }
  END { print count + 0 }
' "$layout/alpha"/_kio_stub_*.pyi)
if [ "$alpha_first_declarations" -ne 256 ] || [ "$alpha_all_declarations" -le 256 ]; then
  printf 'pyright-strict-selftest: malformed cross-shard fixture (%s / %s declarations)\n' \
    "$alpha_first_declarations" "$alpha_all_declarations" >&2
  exit 1
fi

(cd "$layout" && "$PYRIGHT" --project pyrightconfig.json \
    consumer.py huge_protocol.pyi huge_typed_dict.pyi) \
  >"$fixture/layout-positive.log" 2>&1 || {
    printf 'pyright-strict-selftest: cross-shard positive fixture failed\n' >&2
    cat "$fixture/layout-positive.log" >&2
    exit 1
  }
if (cd "$layout" && "$PYRIGHT" --project pyrightconfig.json consumer_negative.py) \
    >"$fixture/layout-negative.log" 2>&1; then
  printf 'pyright-strict-selftest: cross-shard identity mismatch passed\n' >&2
  exit 1
fi
if ! grep -Fq 'not assignable' "$fixture/layout-negative.log"; then
  printf 'pyright-strict-selftest: cross-shard negative failed unexpectedly\n' >&2
  cat "$fixture/layout-negative.log" >&2
  exit 1
fi

# Replay the bounded layout proof with the repository's second checker.
if command -v mise >/dev/null 2>&1 &&
   MYPY=$(cd "$REPO_ROOT" && mise which mypy 2>/dev/null) &&
   [ -n "$MYPY" ]; then
  :
elif command -v mypy >/dev/null 2>&1; then
  MYPY=mypy
else
  printf 'pyright-strict-selftest: required mypy is unavailable; install core mise tools\n' >&2
  exit 2
fi
case $("$MYPY" --version) in
  'mypy 2.4.0 '*) ;;
  *)
    printf 'pyright-strict-selftest: required mypy version is 2.4.0\n' >&2
    exit 2
    ;;
esac
(cd "$layout" && "$MYPY" --strict --python-version 3.10 --no-incremental \
    --cache-dir="$fixture/mypy-cache" consumer.py) \
  >"$fixture/mypy-positive.log" 2>&1 || {
    printf 'pyright-strict-selftest: mypy cross-shard positive fixture failed\n' >&2
    cat "$fixture/mypy-positive.log" >&2
    exit 1
  }
if (cd "$layout" && "$MYPY" --strict --python-version 3.10 --no-incremental \
    --cache-dir="$fixture/mypy-cache" consumer_negative.py) \
    >"$fixture/mypy-negative.log" 2>&1; then
  printf 'pyright-strict-selftest: mypy cross-shard identity mismatch passed\n' >&2
  exit 1
fi
if ! grep -Fq 'Incompatible types in assignment' "$fixture/mypy-negative.log"; then
  printf 'pyright-strict-selftest: mypy cross-shard negative failed unexpectedly\n' >&2
  cat "$fixture/mypy-negative.log" >&2
  exit 1
fi

PYTHONPATH=$layout python3 "$layout/runtime_probe.py"
