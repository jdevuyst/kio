#!/bin/sh
# SUBJECT: Exact Go Value views preserve role ownership, opaque nominal access, nil atoms and static host-constructor indices.
# CONTRACT: specs/backends/go.md, Abstract values and typed views.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-values.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
mkdir -p "$scratch/host/artifact" "$scratch/host/.gocache" "$scratch/host/.gotmp"
cp "$scratch/workdir/out/go/"*.go "$scratch/host/artifact/"
cd "$scratch/host"
export GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local GOWORK=off GOPROXY=off GOSUMDB=off
export GOCACHE="$scratch/host/.gocache" GOTMPDIR="$scratch/host/.gotmp"
go run .
for negative in wrong_index private_payload; do
  cp "$negative.go.txt" negative.go
  if go build -o negative . >negative.log 2>&1; then
    echo "$negative unexpectedly compiled" >&2
    exit 1
  fi
  case "$negative" in
    wrong_index) grep -q 'cannot use.*box' negative.log ;;
    private_payload) grep -q 'unexported field body' negative.log ;;
  esac
  rm negative.go
done
