#!/bin/sh
#
# Prove the emissions orchestrator forwards backend-cohort exclusions,
# per-backend sampling, and caller selectors without widening them. A second
# fixture runs ci/run-tests.sh against fake commands to prove a cap of one is
# applied independently to each top-level backend bucket.
#
# POSIX sh only.

set -eu

unset \
  KIO_CI_SCHEDULE \
  KIO_CI_SCHEDULE_DIR \
  KIO_CI_SCHEDULE_HELD \
  KIO_CI_SCHEDULE_JOBS \
  KIO_CI_SCHEDULE_COMPILER_JOBS \
  KIO_CI_SCHEDULER_BIN \
  KIO_CI_SERIALIZE_CARGO \
  RUSTC_WRAPPER \
  RUSTC_WORKSPACE_WRAPPER \
  CARGO_BUILD_RUSTC_WRAPPER \
  CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
  KIO_TEST_RUNNER_COMPILER_WRAPPER

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
EMISSIONS_SH=$REPO_ROOT/ci/checks/orchestrators/emissions-tests.sh
COMMON_SH=$REPO_ROOT/ci/checks/orchestrators/lib/common.sh
RUN_TESTS_SH=$REPO_ROOT/ci/run-tests.sh

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/emissions-sampling-selftest.XXXXXX") || {
  printf 'emissions-sampling-selftest: cannot create scratch directory\n' >&2
  exit 2
}
trap 'rm -rf "$scratch"' EXIT INT TERM HUP

fixture=$scratch/repo
mkdir -p \
  "$fixture/ci/checks/orchestrators/lib" \
  "$fixture/ci/infra" \
  "$fixture/kio-rs" \
  "$fixture/test-data/emissions" \
  "$scratch/tmp"
cp "$EMISSIONS_SH" "$fixture/ci/checks/orchestrators/emissions-tests.sh"
cp "$COMMON_SH" "$fixture/ci/checks/orchestrators/lib/common.sh"

cat >"$fixture/ci/checks/orchestrators/lib/emissions-contract.sh" <<'EOF'
EMISSIONS_BACKENDS='js ts python java rust go swift haskell'
emissions_validate_corpus() {
  :
}
EOF

cat >"$fixture/ci/infra/sccache.sh" <<'EOF'
kio_configure_sccache_environment() {
  :
}
kio_ensure_sccache_ready() {
  :
}
EOF

cat >"$fixture/ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
  --prepare)
    [ "$#" -eq 1 ] || exit 97
    printf '%s\n' "$KIO_TEST_FAKE_SCHEDULER"
    ;;
  --readiness)
    [ "${2:-}" = -- ] && [ "$#" -gt 2 ] || exit 97
    ;;
  --resource)
    [ "${2:-}" = cargo ] && [ "${3:-}" = -- ] && [ "$#" -gt 3 ] || exit 97
    shift 3
    export KIO_CI_SCHEDULE_HELD=cargo
    exec "$@"
    ;;
  *) exit 97 ;;
esac
EOF

cat >"$fixture/ci/fake-scheduler" <<'EOF'
#!/bin/sh
exit 98
EOF

cat >"$fixture/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = build ] && [ "$2" = --target-dir ]
target_dir=$3
shift 3
[ "$*" = '--all-features --bins' ]
[ "${KIO_CI_SCHEDULE_HELD:-}" = cargo ]
mkdir -p "$target_dir/debug"
printf '#!/bin/sh\nprintf "snapshot-cli\\n"\n' >"$target_dir/debug/kio"
chmod +x "$target_dir/debug/kio"
EOF

cat >"$fixture/ci/run-tests.sh" <<'EOF'
#!/bin/sh
set -eu
for arg in "$@"; do
  case $arg in
    --impl-def=*)
      compiler=${arg#*,kio=}
      compiler=${compiler%%,*}
      [ "$compiler" = "$ORCHESTRATOR_TMP/kio" ]
      [ "$("$compiler")" = snapshot-cli ]
      mkdir -p kio-rs/target/debug
      printf '#!/bin/sh\nexit 92\n' >kio-rs/target/debug/kio
      printf '#!/bin/sh\nexit 93\n' >kio-rs/target/kio-corpus-tools/kio-lsp-cli/debug/kio
      [ "$("$compiler")" = snapshot-cli ]
      ;;
  esac
done
printf '%s\n' "$@" >"$KIO_TEST_RUN_TESTS_ARGS"
EOF
chmod +x "$fixture/ci/schedule.sh" "$fixture/ci/fake-scheduler"

captured=$scratch/orchestrator.args
if ! TMPDIR="$scratch/tmp" \
  KIO_TEST_FAKE_SCHEDULER="$fixture/ci/fake-scheduler" \
  KIO_TEST_RUN_TESTS_ARGS="$captured" \
  sh "$fixture/ci/checks/orchestrators/emissions-tests.sh" \
    --impls=kio@js,kio@rust \
    --sample-cases \
    --case-seed=fixed \
    -- '^rust/exact_case$' '^js/other_case$' \
    >"$scratch/orchestrator.log" 2>&1; then
  cat "$scratch/orchestrator.log" >&2
  printf 'emissions-sampling-selftest: isolated orchestrator fixture failed\n' >&2
  exit 1
fi

for required in \
  '--cases-dir=test-data/emissions' \
  '--sample-cases=1' \
  '--case-seed=fixed'; do
  required_count=$(grep -Fxc -- "$required" "$captured" || true)
  if [ "$required_count" -ne 1 ]; then
    cat "$captured" >&2
    printf 'emissions-sampling-selftest: expected one forwarded %s argument\n' \
      "$required" >&2
    exit 1
  fi
done

impl_count=$(grep -c -- '^--impl-def=' "$captured" || true)
if [ "$impl_count" -ne 2 ] ||
   ! grep -q -- '^--impl-def=name=kio@js,.*target=js$' "$captured" ||
   ! grep -q -- '^--impl-def=name=kio@rust,.*target=rust$' "$captured"; then
  cat "$captured" >&2
  printf 'emissions-sampling-selftest: selected implementation cohort drifted\n' >&2
  exit 1
fi

grep '^--exclude=' "$captured" >"$scratch/actual.excludes" || true
cat >"$scratch/expected.excludes" <<'EOF'
--exclude=^ts/
--exclude=^python/
--exclude=^java/
--exclude=^go/
--exclude=^swift/
--exclude=^haskell/
EOF
if ! diff -u "$scratch/expected.excludes" "$scratch/actual.excludes"; then
  printf 'emissions-sampling-selftest: backend-cohort exclusions drifted\n' >&2
  exit 1
fi

cat >"$scratch/expected.filters" <<'EOF'
^rust/exact_case$
^js/other_case$
EOF
# Every orchestrator-generated argument is an option. Extracting every
# non-option therefore proves these are the complete positive-filter list,
# not merely the final two arguments after a silently added widening filter.
grep -v '^--' "$captured" >"$scratch/actual.filters"
if ! diff -u "$scratch/expected.filters" "$scratch/actual.filters"; then
  printf 'emissions-sampling-selftest: caller selectors were changed or widened\n' >&2
  exit 1
fi

# Exercise the receiving sampler itself with two backend buckets. The fake Kio
# command accepts only the harness-owned cache/test/build path, and the fake
# runner records which cases reached the impl tier.
cases=$scratch/cases
fake_bin=$scratch/bin
mkdir -p "$cases" "$fake_bin"

cat >"$fake_bin/kio-fake" <<'EOF'
#!/bin/sh
set -eu
case "${1:-}" in
  cache)
    case "${2:-}" in
      --help|clear) ;;
      *) exit 92 ;;
    esac
    ;;
  test)
    ;;
  build)
    [ "$#" -eq 2 ]
    mkdir -p "out/$2"
    ;;
  *)
    exit 91
    ;;
esac
EOF

cat >"$fake_bin/runner-fake" <<'EOF'
#!/bin/sh
set -eu
case_path=${PWD%/workdir}
case_name=${case_path##*/}
backend_path=${case_path%/*}
backend=${backend_path##*/}
printf '%s/%s\n' "$backend" "$case_name" >>"$KIO_TEST_SELECTION_LOG"
EOF
chmod +x "$fake_bin/kio-fake" "$fake_bin/runner-fake"

make_case() {
  mc_backend=$1
  mc_name=$2
  mc_case=$cases/$mc_backend/$mc_name
  mkdir -p "$mc_case/workdir"
  cat >"$mc_case/workdir/$mc_name.pkg.kio" <<EOF
build {
  target $mc_backend {
    out "out/$mc_backend/";
  }
}
EOF
  : >"$mc_case/run.args"
  : >"$mc_case/expected.stdout"
  : >"$mc_case/expected.stderr.ignore"
  printf '0\n' >"$mc_case/expected.exit"
}

for backend in js rust; do
  for name in alpha beta gamma; do
    make_case "$backend" "$name"
  done
done

for repeat in one two; do
  selection=$scratch/selection-$repeat
  : >"$selection"
  if ! TMPDIR="$scratch/tmp" \
    KIO_CI_SCHEDULE=DISABLE \
    KIO_DEBUG_PROGRESS_INTERVAL=0 \
    KIO_TEST_SELECTION_LOG="$selection" \
    RUSTC_WRAPPER='' \
    RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' \
    CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
    sh "$RUN_TESTS_SH" \
      --cases-dir="$cases" \
      --cache-base="$scratch/cache-$repeat" \
      --jobs=1 \
      --compiler-jobs=1 \
      --sample-cases=1 \
      --case-seed=fixed \
      --impl-def="name=js,kio=$fake_bin/kio-fake,runner=$fake_bin/runner-fake,target=js" \
      --impl-def="name=rust,kio=$fake_bin/kio-fake,runner=$fake_bin/runner-fake,target=rust" \
      >"$scratch/run-tests-$repeat.log" 2>&1; then
    cat "$scratch/run-tests-$repeat.log" >&2
    printf 'emissions-sampling-selftest: run-tests backend-bucket fixture failed\n' >&2
    exit 1
  fi
done

selection_count=$(wc -l <"$scratch/selection-one" | tr -d ' ')
js_count=$(grep -c '^js/' "$scratch/selection-one" || true)
rust_count=$(grep -c '^rust/' "$scratch/selection-one" || true)
if [ "$selection_count" -ne 2 ] || [ "$js_count" -ne 1 ] ||
   [ "$rust_count" -ne 1 ]; then
  cat "$scratch/selection-one" >&2
  printf 'emissions-sampling-selftest: cap 1 did not select one case per backend bucket\n' >&2
  exit 1
fi
if ! cmp -s "$scratch/selection-one" "$scratch/selection-two"; then
  printf 'emissions-sampling-selftest: fixed seed did not reproduce backend draws\n' >&2
  exit 1
fi

printf 'emissions-sampling-selftest: ok (cohort exclusions, exact filters, and one draw per backend)\n'
