#!/bin/sh
# SUBJECT: Go semantic indices bind public generic construction, including recursive and opaque higher-kinded applications.
# CONTRACT: specs/backends/go.md, Package API and FFI surface.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-indices.XXXXXX")
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

for source in "$scratch/host/"*.go.txt; do
  name=${source##*/}
  cp "$source" negative.go
  if go build -o "$scratch/negative" . >"$scratch/negative.log" 2>&1; then
    printf '%s unexpectedly compiled\n' "$name" >&2
    exit 1
  fi
  case "$name" in
    wrong_kind.go.txt|wrong_host_kind.go.txt) diagnostic='does not satisfy.*KioIndexKind' ;;
    private_constructor.go.txt) diagnostic='undefined.*CallExp_Api__Hidden_make' ;;
    private_projector.go.txt) diagnostic='undefined.*CallExp_Api__Box_open' ;;
    hidden_witness.go.txt) diagnostic='not enough type arguments|too many type arguments' ;;
    explicit_*.go.txt) diagnostic='cannot convert' ;;
    *) diagnostic='cannot use|does not match inferred type' ;;
  esac
  if ! grep -E "$diagnostic" "$scratch/negative.log" >/dev/null; then
    printf '%s failed outside its intended static constraint\n' "$name" >&2
    cat "$scratch/negative.log" >&2
    exit 1
  fi
  rm negative.go
done
printf '%s\n' 'invalid semantic indices, kinds, conversions and private access rejected'
