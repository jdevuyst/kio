#!/bin/sh
#
# Verify the kio-rs implementation: format check, clippy, unit tests,
# plus per-feature check builds that confirm the kio / kio-prime
# split is clean (kio-prime-refactor.md Stage 3.1).
#
# All-features build: produces both the `kio` and `kio-prime`
# binaries from one cargo invocation and runs the complete behavioral
# test suite. Test code must not be gated on `not(feature = ...)`;
# otherwise an all-features run could miss a test that only exists in
# a reduced feature configuration.
#
# Reduced feature checks: confirm the pure compiler slices compile
# independently — the kio-only modules (`desugar`, `label_elab`,
# `dnf`, `lift`, `substitute`, `typecheck_full`, `full`) are
# `#[cfg(feature = "surface")]`-gated and the kio-prime-only `prime`
# module is `#[cfg(feature = "prime")]`-gated. The `prime,cli` check
# also builds the reduced kio-prime binary slice; Cargo skips that
# binary from a prime-only check because its `required-features` include
# `cli`. The `repl-core` check keeps the terminal-free inspector and its
# tests independent of the fuller `repl` feature's terminal dependencies.
#
# Denying rustc warnings on those checks catches dead code in each
# reduced feature configuration. The all-features `cargo test`
# below already runs the complete behavioral test suite.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT/kio-rs"

sh "$REPO_ROOT/ci/cargo.sh" fmt --check

# rg must exist, or the `|| true` below swallows exit 127 and the gate
# passes vacuously. Pinned in mise.toml.
command -v rg >/dev/null 2>&1 || {
  printf '%s\n' "hygiene/kio-rs: rg (ripgrep) not found — the negative-feature-gate check cannot run (pinned in mise.toml; run mise install)." >&2
  exit 1
}

negative_feature_test_gates=$(
  {
    rg -n 'not\s*\(\s*feature\s*=' tests || true
    rg -n '#\[(cfg|cfg_attr)\([^]]*test[^]]*not\s*\(\s*feature\s*=|#\[(cfg|cfg_attr)\([^]]*not\s*\(\s*feature\s*=[^]]*test' src || true
  }
)
if [ -n "$negative_feature_test_gates" ]; then
  printf '%s\n' "negative feature gates are not allowed in kio-rs tests:" >&2
  printf '%s\n' "$negative_feature_test_gates" >&2
  exit 1
fi

sh "$REPO_ROOT/ci/cargo.sh" clippy --all-features --all-targets -- -D warnings

# Retain both compiler executables for the subprocess integration harnesses.
# shellcheck disable=SC1091
. "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh"
init_orchestrator_tmp kio-rs-tests
trap 'rm -rf "$ORCHESTRATOR_TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$ORCHESTRATOR_TMP/kio" \
  --all-features --bins
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio-prime "$ORCHESTRATOR_TMP/kio-prime" \
  --all-features --bins
KIO_DEBUG_TEST_KIO_BIN=$ORCHESTRATOR_TMP/kio
KIO_DEBUG_TEST_KIO_PRIME_BIN=$ORCHESTRATOR_TMP/kio-prime
export KIO_DEBUG_TEST_KIO_BIN KIO_DEBUG_TEST_KIO_PRIME_BIN

sh "$REPO_ROOT/ci/cargo.sh" test --all-features

# Per-feature checks — confirm each binary's slice compiles and has
# no rustc warnings in its reduced feature configuration.
# Per kio-prime-refactor.md Stage 3.1.
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings" \
  sh "$REPO_ROOT/ci/cargo.sh" check --no-default-features --features surface --all-targets
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings" \
  sh "$REPO_ROOT/ci/cargo.sh" check --no-default-features --features prime --all-targets
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings" \
  sh "$REPO_ROOT/ci/cargo.sh" check --no-default-features --features prime,cli --all-targets
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-D warnings" \
  sh "$REPO_ROOT/ci/cargo.sh" check --no-default-features --features repl-core --all-targets
