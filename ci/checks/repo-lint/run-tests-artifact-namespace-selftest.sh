#!/bin/sh

# The harness selects target-qualified namespace mappings before the runner
# boundary, while its identity proxy supplies the source package name.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
RUN_TESTS_SH=${RUN_TESTS_SH:-"$REPO_ROOT/ci/run-tests.sh"}

# Every nested harness invocation is privately captured below. Do not let an
# inherited outer live-progress descriptor bypass those captures; this script
# reports unexpected results through its own diagnostics.
KIO_CI_PROGRESS_FD=
KIO_CI_TASK_NAME=
export KIO_CI_PROGRESS_FD KIO_CI_TASK_NAME

scratch=$(mktemp -d)
cleanup() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$scratch/bin" "$scratch/cases/identity/workdir" "$scratch/tmp" \
  "$scratch/cache"

cat >"$scratch/bin/kio" <<'EOF'
#!/bin/sh
case "${1:-} ${2:-}" in
  'cache --help'|'cache clear') exit 0 ;;
esac
case "${1:-}" in
  test) exit 0 ;;
  build)
    mkdir -p "out/$2"
    exit 0
    ;;
esac
exit 97
EOF

cat >"$scratch/bin/runner" <<'EOF'
#!/bin/sh
: >"$RUNNER_CALLED"
printf '%s\n' "$@"
EOF

cat >"$scratch/cases/identity/workdir/pkg.pkg.kio" <<'EOF'
package pkg;

build {
  target js {
    out "out/js/";
  }
}
EOF
cat >"$scratch/cases/identity/run.args" <<'EOF'
--artifact-namespace haskell=Ignored
--artifact-namespace js=Selected
EOF
cat >"$scratch/cases/identity/expected.stdout" <<'EOF'
--package-name
pkg
--artifact-namespace
Selected
out/js
EOF
printf '0\n' >"$scratch/cases/identity/expected.exit"
: >"$scratch/cases/identity/expected.stderr.ignore"
chmod +x "$scratch/bin/kio" "$scratch/bin/runner"

if ! RUNNER_CALLED="$scratch/runner-called" \
  KIO_CI_SCHEDULE=DISABLE TMPDIR="$scratch/tmp" sh "$RUN_TESTS_SH" \
  --cases-dir="$scratch/cases" \
  --cache-base="$scratch/cache" \
  --impl-def="name=fixture,kio=$scratch/bin/kio,runner=$scratch/bin/runner,target=js" \
  --jobs=1 identity >"$scratch/run.log" 2>&1; then
  cat "$scratch/run.log" >&2
  printf 'run-tests-artifact-namespace-selftest: target selection or identity proxy failed\n' >&2
  exit 1
fi
if [ ! -f "$scratch/runner-called" ]; then
  printf 'run-tests-artifact-namespace-selftest: control did not invoke runner\n' >&2
  exit 1
fi

for package_arg in '--package-name nested' '--package-name=nested'; do
  printf '%s\n' "$package_arg" >"$scratch/cases/identity/run.args"
  rm -f "$scratch/runner-called"
  if RUNNER_CALLED="$scratch/runner-called" \
    KIO_CI_SCHEDULE=DISABLE TMPDIR="$scratch/tmp" sh "$RUN_TESTS_SH" \
      --cases-dir="$scratch/cases" \
      --cache-base="$scratch/cache" \
      --impl-def="name=fixture,kio=$scratch/bin/kio,runner=$scratch/bin/runner,target=js" \
      --jobs=1 identity >"$scratch/reject.log" 2>&1; then
    printf 'run-tests-artifact-namespace-selftest: run.args package descriptor was accepted: %s\n' \
      "$package_arg" >&2
    exit 1
  fi
  if ! grep -Fq 'run.args: --package-name is harness-owned' "$scratch/reject.log"; then
    cat "$scratch/reject.log" >&2
    printf 'run-tests-artifact-namespace-selftest: package descriptor rejection was not diagnosed\n' >&2
    exit 1
  fi
  if [ -f "$scratch/runner-called" ]; then
    printf 'run-tests-artifact-namespace-selftest: rejected package descriptor reached runner\n' >&2
    exit 1
  fi
done

# Optional cache-kind and Prime fields must survive impl plumbing independently.
cat >"$scratch/cache-kind-check.sh" <<'EOF'
#!/bin/sh
# ROUTING: impl
# REQUIRES: runner
set -eu
[ "${KIO_TEST_RUNNER_CACHE_KIND:-}" = "$EXPECTED_CACHE_KIND" ]
case "$EXPECTED_PRIME" in
  absent) [ -z "${KIO_PRIME_BIN:-}" ] ;;
  present) [ -x "${KIO_PRIME_BIN:-}" ] ;;
esac
: >"$CACHE_KIND_CHECK_RAN"
EOF
chmod +x "$scratch/cache-kind-check.sh"
cat >"$scratch/cases/identity/run.args" <<'EOF'
--artifact-namespace js=Selected
EOF
for cache_presence in absent present; do
  for prime_presence in absent present; do
    impl_spec="name=fixture,kio=$scratch/bin/kio,runner=$scratch/bin/runner,target=js"
    expected_cache=
    if [ "$cache_presence" = present ]; then
      impl_spec=$impl_spec,runner-cache-kind=fixture-cache
      expected_cache=fixture-cache
    fi
    if [ "$prime_presence" = present ]; then
      impl_spec=$impl_spec,prime-kio="$scratch/bin/kio"
    fi
    marker="$scratch/cache-kind-check-$cache_presence-$prime_presence"
    if ! CACHE_KIND_CHECK_RAN="$marker" EXPECTED_CACHE_KIND="$expected_cache" \
      EXPECTED_PRIME="$prime_presence" RUNNER_CALLED="$scratch/runner-called" \
      KIO_CI_SCHEDULE=DISABLE TMPDIR="$scratch/tmp" sh "$RUN_TESTS_SH" \
        --cases-dir="$scratch/cases" \
        --cache-base="$scratch/cache" \
        --impl-def="$impl_spec" \
        --check="$scratch/cache-kind-check.sh" \
        --jobs=1 identity >"$scratch/cache-kind.log" 2>&1; then
      cat "$scratch/cache-kind.log" >&2
      printf 'run-tests-artifact-namespace-selftest: optional impl fields crossed (%s cache, %s Prime)\n' \
        "$cache_presence" "$prime_presence" >&2
      exit 1
    fi
    if [ ! -f "$marker" ]; then
      printf 'run-tests-artifact-namespace-selftest: optional-field plumbing check did not run\n' >&2
      exit 1
    fi
  done
done

printf 'run-tests-artifact-namespace-selftest: ok\n'
