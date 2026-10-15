#!/bin/sh
# SUBJECT: Swift application carriers preserve nominal opacity and host identity across partial applications.
# CONTRACT: specs/backends/swift.md, Package API and Host record contract.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift-applications.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

out="$scratch/workdir/out/swift"
host_build="$scratch/host-build"
mkdir -p "$host_build/.module-cache"
swiftc \
  -Onone \
  -module-name CarrierControls \
  -module-cache-path "$host_build/.module-cache" \
  -emit-module \
  -emit-module-path "$host_build/CarrierControls.swiftmodule" \
  -emit-library \
  -static \
  -o "$host_build/libCarrierControls.a" \
  "$out"/*.swift
swiftc \
  -Onone \
  -module-cache-path "$host_build/.module-cache" \
  -I "$host_build" \
  -L "$host_build" \
  -lCarrierControls \
  "$scratch/host/driver.swift" \
  -o "$host_build/driver"
"$host_build/driver"

reject_client() {
  client=$1
  diagnostic=$2
  log="$host_build/$client.stderr"
  if swiftc -typecheck \
    -module-cache-path "$host_build/.module-cache" \
    -I "$host_build" \
    "$scratch/host/$client.swift" >"$host_build/$client.stdout" 2>"$log"; then
    printf 'application_carrier_visibility: %s unexpectedly compiled\n' "$client" >&2
    exit 1
  fi
  # The positive driver compiles the same opaque value with host One. Swift
  # 6.0 diagnoses the wrong-host assignment as an inference error instead.
  if [ "$client" = host_rebrand ] &&
      grep -Eq '(^|/)host_rebrand\.swift:7:[0-9]+: error: type of expression is ambiguous without a type annotation$' "$log" &&
      grep -Fq '7 | let wrong: KioNewtype_api__Opaque<Two, Int, String> = value' "$log"; then
    return
  fi
  if ! grep -Fq "$diagnostic" "$log"; then
    printf 'application_carrier_visibility: %s failed for an unrelated reason\n' "$client" >&2
    cat "$log" >&2
    exit 1
  fi
}

reject_client raw_partial "error: initializer 'init(_:)' requires the types 'KioApply1<KioNewtypeMk_api__Opaque<One>, Int>' and 'KioNative<KioHostTypeIdentity_api__Box>' be equivalent"
reject_client payload_read "error: instance method 'value(as:)' requires the types 'KioApply1<KioNewtypeMk_api__Opaque<One>, Int>' and 'KioApply1<KioNative<KioHostTypeIdentity_api__Native>, Int>' be equivalent"
reject_client witness_forge "error: 'KioNativeConstructor<F>' initializer is inaccessible"
reject_client host_rebrand "arguments to generic parameter 'H' ('One' and 'Two') are expected to be equal"
