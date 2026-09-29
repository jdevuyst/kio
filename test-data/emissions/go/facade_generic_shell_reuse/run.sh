#!/bin/sh
# SUBJECT: Go generic product and sum shells stay source-stable under unrelated additions and nominally distinct across namespaces.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf \
  "$scratch/workdir/out" \
  "$scratch/workdir/namespace_alpha_extended/out" \
  "$scratch/workdir/namespace_beta/out"
cmp "$scratch/workdir/api.kio" "$scratch/workdir/namespace_beta/api.kio" >/dev/null || {
  printf 'generic-shell namespace fixture: API modules differ\n' >&2
  exit 1
}

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
for namespace in namespace_alpha_extended namespace_beta; do
  (
    cd "$scratch/workdir/$namespace"
    "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  )
done

stage="$scratch/driver"
mkdir -p \
  "$stage/shell_alpha" \
  "$stage/shell_beta" \
  "$stage/.gocache" \
  "$stage/.gomodcache" \
  "$stage/.gotmp" \
  "$stage/.telemetry" \
  "$stage/.tmp"

cp "$scratch/workdir/namespace_beta/out/go/"*.go "$stage/shell_beta/"
cp "$scratch/host/host_driver.go.mod" "$stage/go.mod"
cp "$scratch/host/"*.go "$stage/"

install_alpha_artifact() {
  namespace=$1
  rm -f "$stage/shell_alpha/"*.go
  cp "$scratch/workdir/$namespace/out/go/"*.go "$stage/shell_alpha/"
}

run_go() {
  GOENV=off \
    GOEXPERIMENT='' \
    GOTOOLCHAIN=local \
    GOWORK=off \
    GOPROXY=off \
    GOSUMDB=off \
    CGO_ENABLED=0 \
    GOCACHE="$stage/.gocache" \
    GOMODCACHE="$stage/.gomodcache" \
    GOTMPDIR="$stage/.gotmp" \
    TEST_TELEMETRY_DIR="$stage/.telemetry" \
    TMPDIR="$stage/.tmp" \
    go "$@"
}

for namespace in . namespace_alpha_extended; do
  install_alpha_artifact "$namespace"
  (
    cd "$stage"
    run_go run .
  )
done

expect_nominal_rejection() {
  tag=$1
  subject=$2
  stderr_file="$stage/$tag.stderr"

  if (
    cd "$stage"
    run_go build -tags "$tag" -o "$stage/$tag.bin" .
  ) >"$stage/$tag.stdout" 2>"$stderr_file"; then
    printf '%s unexpectedly compiled\n' "$tag" >&2
    exit 1
  fi

  if ! grep -F "$subject" "$stderr_file" >/dev/null \
      || ! grep -F 'shell_alpha.' "$stderr_file" >/dev/null \
      || ! grep -F 'shell_beta.' "$stderr_file" >/dev/null; then
      printf '%s failed for a reason other than cross-namespace sum identity\n' "$tag" >&2
      cat "$stderr_file" >&2
      exit 1
  fi
}

expect_nominal_rejection assignment_negative 'in assignment'
expect_nominal_rejection conversion_negative 'cannot convert'
