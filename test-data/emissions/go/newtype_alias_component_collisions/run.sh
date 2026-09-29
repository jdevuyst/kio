#!/bin/sh
# SUBJECT: Go facade aliases keep newtype and exported-function component collisions distinct.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

out="$scratch/workdir/out/go"
stage="$scratch/driver"
mkdir -p "$stage/artifact" "$stage/.gocache" "$stage/.gotmp"
cp "$out"/*.go "$stage/artifact/"
cp "$scratch/host/main.go" "$stage/main.go"

cat >"$stage/go.mod" <<'EOF'
module driver

go 1.26
EOF

(
  cd "$stage"
  GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local GOWORK=off \
    GOPROXY=off GOSUMDB=off \
    TEST_TELEMETRY_DIR="$stage/.telemetry" \
    GOCACHE="$stage/.gocache" GOTMPDIR="$stage/.gotmp" go run .
)
