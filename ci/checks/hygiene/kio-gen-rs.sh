#!/bin/sh
#
# Verify the kio-gen test artifact: format check, clippy, unit tests.
#
# kio-gen is a private (publish=false) Rust crate at ci/infra/kio-gen-rs/
# that generates Kio' programs for differential testing.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT/ci/infra/kio-gen-rs"

sh "$REPO_ROOT/ci/cargo.sh" fmt --check
sh "$REPO_ROOT/ci/cargo.sh" clippy --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" test
