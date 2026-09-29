#!/bin/sh
# SUBJECT: Go marks retained-only aliases, shells, and constructors with standard deprecation directives.
# CONTRACT: specs/backends/go.md § Deprecated host items.
# Literal backticks below quote generated declarations in failure diagnostics.
# shellcheck disable=SC2016
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-deprecated-history.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
rm -rf "$scratch/workdir/out"

cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

assert_go_deprecated() {
  file=$1
  declaration=$2
  awk -v declaration="$declaration" '
    index($0, declaration) && previous ~ /^[[:space:]]*\/\/ Deprecated:/ { found = 1 }
    { previous = $0 }
    END { exit found ? 0 : 1 }
  ' "$file" || {
    printf '%s: `%s` has no immediate Go deprecation directive\n' \
      "$file" "$declaration" >&2
    exit 1
  }
}

for declaration in \
  'type Env_Api__archived_arg1' \
  'func NewEnv_Api__archived_arg1_0' \
  'func NewEnv_Api__archived_arg1_1' \
  'type Env_Api__archived_ret' \
  'func NewEnv_Api__archived_ret_0' \
  'func NewEnv_Api__archived_ret_1' \
  'type Env_Api__archived_ret_0_value' \
  'func NewEnv_Api__archived_ret_0_value_0' \
  'func NewEnv_Api__archived_ret_0_value_1'
do
  assert_go_deprecated out/go/ffi.go "$declaration"
done
for declaration in \
  'type Sum_Row' \
  'type Sum[' \
  'type KioSum_K0_Case' \
  'type KioSum_K1_Case' \
  'func NewKioSum_K0' \
  'func NewKioSum_K1'
do
  assert_go_deprecated out/go/shapes.go "$declaration"
done
