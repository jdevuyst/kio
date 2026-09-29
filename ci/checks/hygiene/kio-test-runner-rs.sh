#!/bin/sh
#
# Verify the kio-test-runner test artifact: format check, clippy, unit tests.
#
# kio-test-runner-rs is a private (publish=false) Rust crate at
# ci/infra/kio-test-runner-rs/ producing one runner binary per
# executable target (kio-test-runner-js, kio-test-runner-ts,
# kio-test-runner-python, kio-test-runner-java,
# kio-test-runner-rust, kio-test-runner-go, kio-test-runner-swift,
# kio-test-runner-haskell,
# kio-test-runner-dyn-load-prime). Used by the
# orchestrators per the per-impl `runner=` slot.
#
# **Per-feature builds, not combined.** The crate is bin-only with bins
# gated on the `js`, `ts`, `python`, `java`, `rust`, `go`, `swift`,
# `haskell`, and `dyn-load-prime` features;
# shared helper modules in `src/shared/` are `#[path]`-included into each
# bin, with items used by one bin gated `#[cfg(feature = "rust")]`, by the
# typed native bins gated `#[cfg(feature = "typed-native")]` (the shared
# canonical-body classifier), or by the js + ts bins
# `#[cfg(any(feature = "js", feature = "ts"))]` (the JS execution engine
# the `js` and `ts` bins share — the `ts` backend's `<pkg>.js` is the JS
# backend's, byte-identical). Cargo's default-features build
# unifies the features into every bin, which would surface those
# cross-bin items as `dead_code` in the bin that doesn't use them.
# Building each feature in isolation keeps the dead_code analysis honest.
# (The dyn-load-prime bin is self-contained — it shares no `src/shared/`
# module — so it carries no such cross-bin items, but it still builds in
# isolation for parity.)
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT/ci/infra/kio-test-runner-rs"

sh "$REPO_ROOT/ci/cargo.sh" fmt --check
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features js --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features ts --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features python --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features rust --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features go --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features java --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features swift --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features haskell --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" clippy --no-default-features --features dyn-load-prime --all-targets -- -D warnings
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features js
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features ts
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features python
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features rust
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features go
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features java
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features swift
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features haskell
sh "$REPO_ROOT/ci/cargo.sh" test --no-default-features --features dyn-load-prime
