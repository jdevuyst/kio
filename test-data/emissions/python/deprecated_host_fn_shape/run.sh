#!/bin/sh
# SUBJECT: Python omits the complete sealed host-function history from both artifacts while retaining the exact current surface.
# CONTRACT: specs/backends/python.md § Deprecated host items
# Literal backticks below quote generated declarations in failure diagnostics.
# shellcheck disable=SC2016
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-deprecated-shape.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

app_py=out/python/app.py
app_stub=out/python/app
if grep -qF 'archived' "$app_py" || grep -qF 'log' "$app_py" ||
   grep -R -qF 'archived' "$app_stub" || grep -R -qF 'log' "$app_stub"; then
  printf 'Python artifacts: removed host history must be absent\n' >&2
  exit 1
fi

for declaration in \
  '"frame":"V1_M1_C3_apiN3_Str"' \
  '"KioHostIn_"' \
  '"KioHostOut_"' \
  '"name":"open"' \
  '"name":"echo"'
do
  grep -qF "$declaration" "$app_py" || {
    printf 'app.py: missing live exact Python declaration `%s`\n' "$declaration" >&2
    exit 1
  }
done

for declaration in \
  "KioHost_V1_M1_C3_apiN3_Str = typing.TypeVar('KioHost_V1_M1_C3_apiN3_Str')" \
  'KioHostIn_api_Str' \
  'KioHostOut_api_Str' \
  'def open(self) -> KioProtocol_Host_api_B0: ...' \
  'def echo(self, p0: KioHost_V1_M1_C3_apiN3_Str, /) -> KioHost_V1_M1_C3_apiN3_Str: ...'
do
  grep -R -qF "$declaration" "$app_stub" || {
    printf 'app stub package: missing live exact Python declaration `%s`\n' "$declaration" >&2
    exit 1
  }
done
