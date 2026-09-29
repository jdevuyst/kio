#!/bin/sh
#
# Verify the private Rust CI scheduler.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT/ci/infra/kio-ci-scheduler-rs"

sh "$REPO_ROOT/ci/cargo.sh" fmt --check
sh "$REPO_ROOT/ci/cargo.sh" clippy --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" test
