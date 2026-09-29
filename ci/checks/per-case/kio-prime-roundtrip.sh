#!/bin/sh
# ROUTING: impl
# REQUIRES: prime-kio
#
# Per-case deterministic-compilation check. For every case that the selected
# implementation can build, this check compares two routes to the same target:
#
#   regular source --kio-----------> target artifact tree
#   regular source --kio--> Kio' --kio-prime--> target artifact tree
#
# The second compiler is the separately-built, prime-only `kio-prime` binary.
# The generated trees must contain the same regular files at the same relative
# paths with byte-identical contents. Host binaries are deliberately outside
# the comparison: they add host-toolchain nondeterminism after Kio codegen.
# Every emitted regular module is also checked by the independent Kio' grammar
# verifier before the reduced compiler consumes it.
#
# The runner sets KIO_BIN, KIO_PRIME_BIN, KIO_PRIME_CHECK_BIN, and KIO_TARGET.
# POSIX sh only.

set -eu

for required in KIO_BIN KIO_PRIME_BIN KIO_PRIME_CHECK_BIN KIO_TARGET; do
  eval "value=\${$required:-}"
  if [ -z "$value" ]; then
    printf 'kio-prime-roundtrip: %s is not set\n' "$required" >&2
    exit 2
  fi
done

if [ ! -d workdir ] || [ ! -f expected.exit ]; then
  exit 0
fi

pkg_file=
for candidate in workdir/*.pkg.kio; do
  if [ ! -f "$candidate" ]; then
    continue
  fi
  if [ -n "$pkg_file" ]; then
    exit 0
  fi
  pkg_file=$candidate
done
if [ -z "$pkg_file" ]; then
  exit 0
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)
if [ -n "${TMPDIR:-}" ]; then
  scratch=$(mktemp -d "$TMPDIR/kio-prime-roundtrip.XXXXXX")
else
  mkdir -p "$repo_root/target"
  scratch=$(mktemp -d "$repo_root/target/kio-prime-roundtrip.XXXXXX")
  TMPDIR="$scratch/tmp"
  if ! mkdir "$TMPDIR"; then
    rm -rf "$scratch"
    printf 'kio-prime-roundtrip: cannot create TMPDIR: %s\n' "$TMPDIR" >&2
    exit 1
  fi
  export TMPDIR
fi
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
mkdir "$scratch/case"
cp -RL workdir/. "$scratch/case/"
rm -rf "$scratch/case/out"
scratch_pkg="$scratch/case/$(basename "$pkg_file")"
expected_exit=$(tr -d '[:space:]' <expected.exit)

# Parse the copied manifest before deciding applicability. The helper emits a
# canonical manifest only when a distinct selected target exists, with both
# output roots redirected to check-owned relative paths.
prepare_status=0
"$KIO_BIN" debug kio-prime-roundtrip-package \
  "$scratch_pkg" "$KIO_TARGET" >"$scratch_pkg.tmp" || prepare_status=$?
if [ "$prepare_status" -ne 0 ]; then
  rm -f "$scratch_pkg.tmp"
  # A declared package-parse rejection has no artifact to compare. Only Parse
  # is inapplicable; matching I/O/Internal failures must stay loud.
  if [ "$prepare_status" = 11 ] && [ "$expected_exit" = 11 ]; then
    exit 0
  fi
  printf 'kio-prime-roundtrip: cannot prepare package manifest %s\n' \
    "$scratch_pkg" >&2
  exit 1
fi
if [ ! -s "$scratch_pkg.tmp" ]; then
  rm -f "$scratch_pkg.tmp"
  exit 0
fi
mv "$scratch_pkg.tmp" "$scratch_pkg"
rm -rf "$scratch/case/__kio_roundtrip_target" "$scratch/case/__kio_roundtrip_prime"

package_selector="./$(basename "$scratch_pkg")"
direct="$scratch/case/__kio_roundtrip_target"
emitted="$scratch/case/__kio_roundtrip_prime"
combined_failed=0

# Successful cases can request both artifacts in one cold build. If the
# combined command fails, replay the legacy sequence so target inapplicability
# still skips while a genuine multi-target regression fails closed.
if [ "$expected_exit" = 0 ]; then
  if ! (
    cd "$scratch/case"
    "$KIO_BIN" --no-cache build "$KIO_TARGET" kio-prime "$package_selector"
  ) >"$scratch/combined.log" 2>&1; then
    combined_failed=1
    rm -rf "$direct" "$emitted"
  fi
fi

if [ "$expected_exit" != 0 ] || [ "$combined_failed" -ne 0 ]; then
  if ! (
    cd "$scratch/case"
    "$KIO_BIN" --no-cache build "$KIO_TARGET" "$package_selector"
  ) >"$scratch/direct.log" 2>&1; then
    exit 0
  fi
  if [ ! -d "$direct" ]; then
    printf 'kio-prime-roundtrip: direct build succeeded but %s was not created\n' "$direct" >&2
    exit 1
  fi

  if ! (
    cd "$scratch/case"
    "$KIO_BIN" --no-cache build kio-prime "$package_selector"
  ) >"$scratch/prime.log" 2>&1; then
    printf 'kio-prime-roundtrip: full compiler failed to emit Kio:\n' >&2
    cat "$scratch/prime.log" >&2
    exit 1
  fi
fi

if [ ! -d "$direct" ]; then
  printf 'kio-prime-roundtrip: direct build succeeded but %s was not created\n' "$direct" >&2
  exit 1
fi
if [ ! -d "$emitted" ]; then
  printf 'kio-prime-roundtrip: Kio build succeeded but %s was not created\n' "$emitted" >&2
  exit 1
fi
if [ "$combined_failed" -ne 0 ]; then
  printf 'kio-prime-roundtrip: multi-target build regression; separate builds succeeded:\n' >&2
  cat "$scratch/combined.log" >&2
  exit 1
fi

emitted_modules="$scratch/emitted-modules"
(
  cd "$emitted"
  find . -type f -name '*.kio' \
    ! -name '*.pkg.kio' \
    ! -name '*.dep.kio' \
    ! -name '*.lock.kio' \
    ! -name '*.sig.kio' \
    ! -path './out/*' | LC_ALL=C sort
) >"$emitted_modules"
while IFS= read -r relative || [ -n "$relative" ]; do
  if ! "$KIO_PRIME_CHECK_BIN" "$emitted/$relative" >/dev/null 2>&1; then
    printf 'kio-prime-roundtrip: emitted file does not parse as Kio: %s\n' "$relative" >&2
    "$KIO_PRIME_CHECK_BIN" "$emitted/$relative" >&2 || :
    exit 1
  fi
done <"$emitted_modules"

# Signature history is a package input, not part of Kio'. Preserve it across
# the phase boundary so both routes see the same compatibility contract.
for signature in "$scratch/case"/*.sig.kio; do
  if [ -f "$signature" ]; then
    cp "$signature" "$emitted/"
  fi
done

if ! (
  cd "$emitted"
  "$KIO_PRIME_BIN" --no-cache build "$KIO_TARGET" "$package_selector"
) >"$scratch/reduced.log" 2>&1; then
  printf 'kio-prime-roundtrip: reduced compiler failed to build %s:\n' "$KIO_TARGET" >&2
  cat "$scratch/reduced.log" >&2
  exit 1
fi

reduced="$emitted/__kio_roundtrip_target"
if [ ! -d "$reduced" ]; then
  printf 'kio-prime-roundtrip: reduced build succeeded but %s was not created\n' "$reduced" >&2
  exit 1
fi

for root in "$direct" "$reduced"; do
  if find "$root" -type l | grep -q .; then
    printf 'kio-prime-roundtrip: artifact tree contains a symbolic link: %s\n' "$root" >&2
    exit 1
  fi
done

direct_dirs="$scratch/direct-dirs"
reduced_dirs="$scratch/reduced-dirs"
direct_files="$scratch/direct-files"
reduced_files="$scratch/reduced-files"
(
  cd "$direct"
  find . -mindepth 1 -type d | LC_ALL=C sort
) >"$direct_dirs"
(
  cd "$reduced"
  find . -mindepth 1 -type d | LC_ALL=C sort
) >"$reduced_dirs"
(
  cd "$direct"
  find . -type f | LC_ALL=C sort
) >"$direct_files"
(
  cd "$reduced"
  find . -type f | LC_ALL=C sort
) >"$reduced_files"

if ! cmp -s "$direct_dirs" "$reduced_dirs"; then
  printf 'kio-prime-roundtrip: artifact directory sets differ for %s\n' "$KIO_TARGET" >&2
  diff -u "$direct_dirs" "$reduced_dirs" >&2 || :
  exit 1
fi
if ! cmp -s "$direct_files" "$reduced_files"; then
  printf 'kio-prime-roundtrip: artifact file sets differ for %s\n' "$KIO_TARGET" >&2
  diff -u "$direct_files" "$reduced_files" >&2 || :
  exit 1
fi

while IFS= read -r relative || [ -n "$relative" ]; do
  if ! cmp -s "$direct/$relative" "$reduced/$relative"; then
    printf 'kio-prime-roundtrip: artifact bytes differ for %s: %s\n' \
      "$KIO_TARGET" "$relative" >&2
    cmp -l "$direct/$relative" "$reduced/$relative" | sed -n '1,8p' >&2 || :
    exit 1
  fi
done <"$direct_files"
