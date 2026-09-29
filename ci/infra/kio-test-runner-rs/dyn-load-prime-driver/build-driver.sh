#!/bin/sh
#
# Assemble and build the dyn-load-prime test driver package to JS.
#
# The driver is the `dyn_load_prime` package (the live source under
# test-data/poc/dyn_load_prime/workdir/, the single source of truth — no
# vendored fork) plus this directory's `driver.kio` host module and `dyn_load_prime_driver.pkg.kio`
# manifest. The driver's `main` loads a guest's emitted Kio' image from
# the runner-supplied `read_guest_image` host capability, then follows the
# selected shared protocol's load-only, construct-only, exact
# module-qualified main, or supported export-script mode while observing the
# guest's host effects. The
# dyn-load-prime test runner (kio-test-runner-dyn-load-prime) reuses this one
# compiled JS module across every case.
#
# Building it here, once, mirrors how ci/checks/orchestrators/golden-tests.sh
# builds the kio compiler and the runner crate up front: the orchestrator
# invokes this script, then exports the emitted JS path to the runner via
# KIO_DYN_LOAD_PRIME_DRIVER_JS. The runner reads no `kio build` output of its
# own (decoupling red lines); it reads the driver JS this script names and
# the per-case image text from the case's kio-prime build output.
#
# Usage:
#   sh build-driver.sh <kio-binary> <dyn_load_prime-src-dir> <publication-dir>
#
# On success the absolute path to the emitted driver JS module is printed
# to stdout (and nothing else); all build chatter goes to stderr.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
CI_DIR=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

if [ $# -ne 3 ]; then
  printf 'usage: sh build-driver.sh <kio-binary> <dyn_load_prime-src-dir> <publication-dir>\n' >&2
  exit 2
fi

kio_bin=$1
dyn_load_prime_src=$2
publication_dir=$3

if ! command -v "$kio_bin" >/dev/null 2>&1; then
  printf 'build-driver.sh: kio binary not executable: %s\n' "$kio_bin" >&2
  exit 2
fi
if [ ! -d "$dyn_load_prime_src" ]; then
  printf 'build-driver.sh: dyn_load_prime source dir not found: %s\n' "$dyn_load_prime_src" >&2
  exit 2
fi

if ! command -v git >/dev/null 2>&1; then
  printf 'build-driver.sh: git is required to identify driver inputs\n' >&2
  exit 2
fi

# Each invocation assembles and builds in its own directory. Published files
# are immutable and input-addressed, so a later orchestrator can reuse the
# artifact but can never delete a path an earlier orchestrator has exported.
mkdir -p "$publication_dir/artifacts"
stage=$(mktemp -d "$publication_dir/.stage.XXXXXX") || {
  printf 'build-driver.sh: cannot create staging directory under %s\n' "$publication_dir" >&2
  exit 1
}
cleanup() {
  rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

assembly="$stage/source"
mkdir -p "$assembly"

# Assemble: the dyn_load_prime modules (minus its own manifest), plus this
# directory's driver module and manifest. The package's own worked example
# lives in `testapi/main` and is skipped — the driver supplies its own
# `main` (in `driver`) and replaces that entry. The `out/` build-output
# tree is never copied.
for f in "$dyn_load_prime_src"/*.kio; do
  base=$(basename "$f")
  case "$base" in
    dyn_load_prime.pkg.kio) continue ;;
  esac
  cp "$f" "$assembly/$base"
done
if [ -d "$dyn_load_prime_src/testapi" ]; then
  mkdir -p "$assembly/testapi"
  for f in "$dyn_load_prime_src"/testapi/*.kio; do
    base=$(basename "$f")
    case "$base" in
      main.kio) continue ;;
    esac
    cp "$f" "$assembly/testapi/$base"
  done
fi
# The materialized `list` dependency tree (consumed as ordinary source;
# the copy keeps the assembly buildable without a `kio dep fetch`).
cp -R "$dyn_load_prime_src/list" "$assembly/list"
cp -R "$dyn_load_prime_src/loader" "$assembly/loader"
cp -R "$dyn_load_prime_src/elab" "$assembly/elab"

cp "$SCRIPT_DIR/driver.kio" "$assembly/driver.kio"
cp "$SCRIPT_DIR/dyn_load_prime_driver.pkg.kio" "$assembly/dyn_load_prime_driver.pkg.kio"

# The key covers the compiler, the assembly builder, and every copied input
# with its assembly-relative path. A hit is therefore reusable across
# orchestrators without treating a mutable build directory as a cache entry.
key_manifest="$stage/key-manifest"
compiler_hash=$(git hash-object "$kio_bin")
builder_hash=$(git hash-object "$SCRIPT_DIR/build-driver.sh")
source_paths_unsorted="$stage/source-paths.unsorted"
source_paths="$stage/source-paths"
if ! find "$assembly" -type f -print >"$source_paths_unsorted"; then
  printf 'build-driver.sh: cannot enumerate driver inputs under %s\n' "$assembly" >&2
  exit 1
fi
if ! LC_ALL=C sort "$source_paths_unsorted" >"$source_paths"; then
  printf 'build-driver.sh: cannot sort driver inputs under %s\n' "$assembly" >&2
  exit 1
fi
{
  printf 'dyn-load-prime-driver-publication-v1\n'
  printf 'compiler\t%s\n' "$compiler_hash"
  printf 'builder\t%s\n' "$builder_hash"
  while IFS= read -r file; do
    relative=${file#"$assembly/"}
    file_hash=$(git hash-object "$file")
    printf 'source\t%s\t%s\n' "$file_hash" "$relative"
  done <"$source_paths"
} >"$key_manifest"
key=$(git hash-object "$key_manifest")
published_dir="$publication_dir/artifacts/$key"
published_js="$published_dir/dyn_load_prime_driver.js"
if [ -f "$published_js" ]; then
  printf '%s\n' "$published_js"
  exit 0
fi

(
  cd "$assembly"
  sh "$CI_DIR/schedule.sh" --resource compiler -- "$kio_bin" build js
) >&2

driver_js="$assembly/out/js/dyn_load_prime_driver.js"
if [ ! -f "$driver_js" ]; then
  printf 'build-driver.sh: build succeeded but %s was not created\n' "$driver_js" >&2
  exit 1
fi

# A hard link publishes the complete file atomically without replacing an
# identical-key winner. Staging and publication share a filesystem because
# both live below publication_dir.
chmod a-w "$driver_js"
mkdir -p "$published_dir"
if ! ln "$driver_js" "$published_js" 2>/dev/null; then
  if [ ! -f "$published_js" ]; then
    printf 'build-driver.sh: cannot publish driver JS as %s\n' "$published_js" >&2
    exit 1
  fi
fi

# The immutable emitted JS path is the only thing on stdout.
printf '%s\n' "$published_js"
