#!/bin/sh
#
# Smoke-test the reporting harnesses (under reports/) without running
# their full analyses. Catches harness regressions — cargo-fuzz plugin
# removal, kio-rs/fuzz Cargo.toml breakage, a mutation target that a
# rename left matching nothing, a cargo-llvm-cov flag rename — the
# instant a report tool or the tree drifts, instead of only when someone
# finally invokes the owning audit-* skill (which is how the mutation and
# coverage harnesses each sat broken for weeks).
#
# Each smoke exercises the specific machinery its harness depends on and
# that a plain build does not cover — not just "is the tool installed":
#
#   cargo-fuzz       — `cargo +nightly fuzz list`. Confirms cargo-fuzz
#                      installed, nightly toolchain present, and
#                      kio-rs/fuzz/Cargo.toml lists its targets. (Building
#                      the targets is a full ASAN recompile — too heavy
#                      for a smoke; audit-fuzz builds and runs them.)
#   cargo-mutants    — a dependency-free scope self-test always proves that
#                      every claimed production source is reached and the
#                      optimizer firing inventory matches its real catalog.
#                      When cargo-mutants is installed, one combined
#                      `cargo mutants --list` additionally proves every target
#                      and required firing site has an actual candidate. A
#                      renamed module, missing `/**`, or exclusion can no
#                      longer silently shrink the advertised reach.
#   cargo-llvm-cov   — validates the coverage harnesses' show-env/report flags.
#
# Tool-dependent probes are skipped with a notice — local devs and the core
# devcontainer (including the GitHub CI image) do not source-build these
# optional Cargo tools. Dependency-free checks such as the mutation scope
# inventory still run everywhere. The real candidate-manifest probes add teeth
# on a report-tool-provisioned box and before an audit-* harness. Install them
# on demand with
# `sh ci/impl-toolchain.sh install-report-tools`.
#
# Picked up by ci/all.sh's auto-discovery.
#
# POSIX sh only.

set -eu

if [ $# -gt 0 ]; then
  case "$1" in
    -h|--help)
      cat <<'EOF'
Usage: sh ci/checks/orchestrators/smoke-reports.sh

Smoke-list the auxiliary report-producing tools (cargo-fuzz,
cargo-mutants, …). Doesn't invoke `kio` directly.
EOF
      exit 0
      ;;
    *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
SMOKE_MUTATION_MANIFEST=

cleanup() {
  if [ -n "$SMOKE_MUTATION_MANIFEST" ] && [ -e "$SMOKE_MUTATION_MANIFEST" ]; then
    rm -f "$SMOKE_MUTATION_MANIFEST"
  fi
}

trap cleanup 0
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

printf 'smoke-coverage: running the dependency-free reachability self-test\n'
sh "$REPO_ROOT/reports/equiv-eval-reachability.sh" --self-test
if ! grep -q 'llvm-cov --profile=dev --all-features show-env' "$REPO_ROOT/reports/equiv-eval-reachability.sh" \
  || ! grep -q 'llvm-cov --profile=dev --all-features report' "$REPO_ROOT/reports/equiv-eval-reachability.sh" \
  || ! grep -Fq "[ \"\$COV\" = \"\$COVERAGE_BASE\" ]" "$REPO_ROOT/reports/equiv-eval-reachability.sh" \
  || ! awk '
    /^KIO_CI_SCHEDULER_BIN=\$\($/ { if (pin || block) exit 1; pin = NR; block = 1; next }
    block && /^  unset LLVM_PROFILE_FILE CARGO_LLVM_COV_TARGET_DIR CARGO_LLVM_COV CARGO_LLVM_COV_SHOW_ENV RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTDOCFLAGS CARGO_ENCODED_RUSTDOCFLAGS$/ { if (scrubbed) exit 1; scrubbed = NR; next }
    block && /^  sh "\$REPO_ROOT\/ci\/schedule\.sh" --prepare$/ { if (!scrubbed || prepared) exit 1; prepared = NR; next }
    block && /^\) \|\| exit \$\?$/ { if (!prepared || closed) exit 1; closed = NR; block = 0; next }
    /^export KIO_CI_SCHEDULER_BIN$/ { if (block || !closed || exported) exit 1; exported = NR; next }
    /^eval "\$coverage_env"$/ { if (!exported || seen) exit 1; seen = NR; next }
    /^run_kio bins-build[.]log / { if (!seen || built) exit 1; built = NR; next }
    /^mv "\$COV\/debug\/kio\$exe" "\$ORCHESTRATOR_TMP\/bin\/coverage-kio\$exe"$/ { if (!built || saved_kio) exit 1; saved_kio = NR; next }
    /^mv "\$COV\/debug\/kio-prime\$exe" "\$ORCHESTRATOR_TMP\/bin\/coverage-kio-prime\$exe"$/ { if (!saved_kio || saved_prime) exit 1; saved_prime = NR; next }
    /llvm-cov clean --profraw-only$/ { if (!saved_prime || cleaned) exit 1; cleaned = NR; next }
    /profiles "\$COV" empty$/ { if (!cleaned || emptied) exit 1; emptied = NR; next }
    /^run_kio unit[.]log / { if (!emptied || unit) exit 1; unit = NR; next }
    /^mv "\$ORCHESTRATOR_TMP\/bin\/coverage-kio\$exe" "\$COV\/debug\/kio\$exe"$/ { if (!unit || restored_kio) exit 1; restored_kio = NR; next }
    /^mv "\$ORCHESTRATOR_TMP\/bin\/coverage-kio-prime\$exe" "\$COV\/debug\/kio-prime\$exe"$/ { if (!restored_kio || restored_prime) exit 1; restored_prime = NR; next }
    /^REAL_KIO=\$COV\/debug\/kio\$exe$/ { if (!restored_prime || real_kio) exit 1; real_kio = NR; next }
    END { if (block || !pin || !scrubbed || !prepared || !closed || !exported || !seen || !built || !saved_kio || !saved_prime || !cleaned || !emptied || !unit || !restored_kio || !restored_prime || !real_kio) exit 1 }
  ' "$REPO_ROOT/reports/equiv-eval-reachability.sh"; then
    printf 'smoke-coverage: reachability report coverage setup ordering drifted\n' >&2
    exit 1
fi

smoke_fuzz() {
  if ! command -v cargo-fuzz >/dev/null 2>&1; then
    printf 'smoke-fuzz: cargo-fuzz not installed; skipping\n'
    return 0
  fi
  if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    printf 'smoke-fuzz: nightly toolchain not installed; skipping\n'
    return 0
  fi
  cd "$REPO_ROOT/kio-rs"
  printf 'smoke-fuzz: listing fuzz targets\n'
  sh "$REPO_ROOT/ci/cargo.sh" +nightly fuzz list
  cd - >/dev/null
}

smoke_mutation() {
  printf 'smoke-mutation: checking the declared semantic-core scope\n'
  sh "$REPO_ROOT/reports/mutation.sh" --scope-self-test
  if ! sh "$REPO_ROOT/ci/cargo.sh" mutants --version >/dev/null 2>&1; then
    printf 'smoke-mutation: cargo-mutants not installed; skipping\n'
    return 0
  fi
  cd "$REPO_ROOT/kio-rs"
  printf 'smoke-mutation: checking the real cargo-mutants candidate manifest\n'
  # Read the targets from the harness so the probe and production report cannot
  # drift. `set -f` keeps `**` literal for cargo-mutants to expand.
  _oldifs=$IFS
  set -f
  IFS='
'
  set --
  for _t in $(sh "$REPO_ROOT/reports/mutation.sh" --print-targets); do
    set -- "$@" --file "$_t"
  done
  set +f
  IFS=$_oldifs

  _scope_parent=${TMPDIR:-$REPO_ROOT/target}
  mkdir -p "$_scope_parent"
  SMOKE_MUTATION_MANIFEST=$(mktemp "$_scope_parent/smoke-mutation-scope.XXXXXX") || {
    printf 'smoke-mutation: cannot create candidate manifest\n' >&2
    cd - >/dev/null
    return 1
  }
  if ! sh "$REPO_ROOT/ci/cargo.sh" mutants --list "$@" \
      >"$SMOKE_MUTATION_MANIFEST" 2>/dev/null; then
    printf 'smoke-mutation: cargo-mutants could not list the declared scope\n' >&2
    cd - >/dev/null
    return 1
  fi
  if ! sh "$REPO_ROOT/reports/mutation.sh" \
      --validate-scope-manifest="$SMOKE_MUTATION_MANIFEST"; then
    cd - >/dev/null
    return 1
  fi
  rm -f "$SMOKE_MUTATION_MANIFEST"
  SMOKE_MUTATION_MANIFEST=
  cd - >/dev/null
}

smoke_coverage() {
  if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    printf 'smoke-coverage: cargo-llvm-cov not installed; skipping\n'
    return 0
  fi
  cd "$REPO_ROOT/kio-rs"
  printf 'smoke-coverage: exercising the reachability report llvm-cov flags\n'
  # Exercise the superset of show-env flags used by both coverage reports.
  # Assert it succeeds and emits the vars the harnesses rely on, so a
  # flag rename fails here (instant) rather than as "no input files" minutes
  # into a coverage run. show-env's advisory line goes to stderr (dropped).
  if ! _env=$(sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --profile=dev --all-features \
      show-env --export-prefix --remap-path-prefix 2>/dev/null); then
    printf 'smoke-coverage: cargo llvm-cov show-env flags failed (flag drift?)\n' >&2
    cd - >/dev/null
    return 1
  fi
  if ! printf '%s\n' "$_env" | grep -Eq 'RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS' \
      || ! printf '%s\n' "$_env" | grep -q LLVM_PROFILE_FILE; then
    printf 'smoke-coverage: show-env omitted instrumentation variables\n' >&2
    cd - >/dev/null
    return 1
  fi
  _help=$(sh "$REPO_ROOT/ci/cargo.sh" llvm-cov --profile=dev --all-features report --help 2>/dev/null) || return 1
  for _flag in --lcov --json --failure-mode; do
    printf '%s\n' "$_help" | grep -q -- "$_flag" || {
      printf 'smoke-coverage: cargo llvm-cov report lacks %s\n' "$_flag" >&2
      cd - >/dev/null
      return 1
    }
  done
  printf 'smoke-coverage: reachability report flags OK\n'
  cd - >/dev/null
}

smoke_fuzz
smoke_mutation
smoke_coverage
