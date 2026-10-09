#!/bin/sh
# SUBJECT: A closed polymorphic payload under an unused outer binder preserves inferred views, type-stage effects and captured host context.
# CONTRACT: specs/backends/go.md, Abstract values and typed views.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-closed-stage.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
cp "$scratch/host/go.mod" "$scratch/host/main.go" "$scratch/workdir/"
cd "$scratch/workdir"
"${KIO_BIN:?}" test >/dev/null
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
mkdir -p "$scratch/gotmp"
export GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local GOWORK=off GOPROXY=off GOSUMDB=off
export GOTMPDIR="$scratch/gotmp"
go run .

cp "$scratch/host/explicit_stage_host.go.txt" negative.go
if go build -o "$scratch/negative" . >"$scratch/negative.log" 2>&1; then
  printf '%s\n' 'explicit stage host conversion unexpectedly compiled' >&2
  exit 1
fi
if ! grep -E 'cannot convert closed.*Exp_Api__outer_ret' "$scratch/negative.log" >/dev/null; then
  cat "$scratch/negative.log" >&2
  exit 1
fi
printf '%s\n' 'changing the captured native host binding is rejected'
