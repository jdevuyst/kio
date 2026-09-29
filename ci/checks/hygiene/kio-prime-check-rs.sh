#!/bin/sh
#
# Verify the kio-prime-check test artifact: format check, clippy, unit tests.
#
# kio-prime-check is a private (publish=false) Rust crate at
# ci/infra/kio-prime-check-rs/ that parses Kio' source against the formal
# grammar in specs/prime.md. Used by the per-case prime-marker check to
# verify the IS_KIO_PRIME corpus invariant.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT/ci/infra/kio-prime-check-rs"

sh "$REPO_ROOT/ci/cargo.sh" fmt --check
sh "$REPO_ROOT/ci/cargo.sh" clippy --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" test
