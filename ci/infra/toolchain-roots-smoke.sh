#!/bin/sh
# Prove selection overrides an inherited compiler runtime root, without cache wipes.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
mkdir -p "$work/unfixed/ci" "$work/unfixed/tools" "$work/empty-goroot"
cp "$repo_root/ci/impl-toolchain.sh" "$work/unfixed/ci/"
cp -R "$repo_root/tools/toolchains" "$work/unfixed/tools/"
cp "$repo_root"/mise*.toml "$repo_root"/mise*.lock "$work/unfixed/"
# shellcheck disable=SC2016 # Remove the literal implementation lines in the private copy.
sed '/export GOROOT="\$tc_install"/d; /export RUSTC="\$tc_rustc"/d; /^[[:space:]]*unset PYTHONHOME$/d' "$repo_root/tools/toolchains/provision.sh" >"$work/unfixed/tools/toolchains/provision.sh"
store=$(sh "$repo_root/ci/impl-toolchain.sh" toolchain-plan go | sed -n 's/^store: //p')
printf '%s\n' 'package main' 'import "fmt"' 'func main() { fmt.Println(42) }' >"$work/witness.go"
KIO_TOOLCHAINS_ROOT="$store" GOROOT="$work/empty-goroot" \
  sh "$work/unfixed/ci/impl-toolchain.sh" toolchain-install go
if KIO_TOOLCHAINS_ROOT="$store" GOROOT="$work/empty-goroot" \
    sh "$work/unfixed/ci/impl-toolchain.sh" toolchain-exec go -- \
      go build -o "$work/negative" "$work/witness.go" >"$work/unfixed.log" 2>&1; then
  printf 'error: fix-disabled Go selection unexpectedly compiled with an empty GOROOT\n' >&2; exit 1
fi
grep -q 'fmt.*not in std' "$work/unfixed.log"
printf 'fix-disabled Go selection: rejected poisoned GOROOT as expected\n'
KIO_TOOLCHAINS_ROOT="$store" sh "$work/unfixed/ci/impl-toolchain.sh" toolchain-install rust
if KIO_TEST_DIRECT_CARGO=1 KIO_TEST_TRACE=1 KIO_TOOLCHAINS_ROOT="$store" KIO_TEST_TOOLCHAIN_OWNER="$work/unfixed/ci/impl-toolchain.sh" \
    sh "$repo_root/ci/checks/repo-lint/toolchain-cargo-selftest.sh" >"$work/unfixed-rust.log" 2>&1; then
  printf 'error: fix-disabled Rust selection ignored the poisoned compiler fixture\n' >&2; exit 1
fi
if ! grep -q 'poisoned compiler selected' "$work/unfixed-rust.log"; then
  sed -n '1,120p' "$work/unfixed-rust.log" >&2
  exit 1
fi
printf 'fix-disabled Rust selection: caller compiler poison executed as expected\n'
KIO_TOOLCHAINS_ROOT="$store" sh "$work/unfixed/ci/impl-toolchain.sh" toolchain-install python
if KIO_TOOLCHAINS_ROOT="$store" PYTHONHOME="$work/empty-goroot" \
    sh "$work/unfixed/ci/impl-toolchain.sh" toolchain-exec python -- \
      python3 -c 'import json; print(42)' >"$work/unfixed-python.log" 2>&1; then
  printf 'error: fix-disabled Python selection ignored the poisoned runtime root\n' >&2; exit 1
fi
grep -q 'Python path configuration' "$work/unfixed-python.log"
printf 'fix-disabled Python selection: poisoned runtime root prevented stdlib import as expected\n'
sh "$repo_root/ci/impl-toolchain.sh" toolchain-install go
GOROOT="$work/empty-goroot" sh "$repo_root/ci/impl-toolchain.sh" toolchain-exec go -- \
  go build -o "$work/positive" "$work/witness.go"
[ "$("$work/positive")" = 42 ]
sh "$repo_root/ci/impl-toolchain.sh" toolchain-install rust
sh "$repo_root/ci/checks/repo-lint/toolchain-cargo-selftest.sh"
mkdir "$work/dependency"
printf '%s\n' 'answer = 42' >"$work/dependency/caller_dependency.py"
sh "$repo_root/ci/impl-toolchain.sh" toolchain-install python
PYTHONHOME="$work/empty-goroot" PYTHONPATH="$work/dependency" \
  sh "$repo_root/ci/impl-toolchain.sh" toolchain-exec python -- python3 -c \
    'import os, sys, json, caller_dependency; assert sys.prefix.startswith(os.environ["KIO_TOOLCHAINS_ROOT"] + "/mise/data/installs/python/"); assert json.__file__.startswith(sys.prefix); assert caller_dependency.answer == 42; print(42)'
printf 'toolchain roots smoke: selected Go, Rust/Cargo and Python passed poisoned inherited roots with caller dependencies/wrapper preserved\n'
