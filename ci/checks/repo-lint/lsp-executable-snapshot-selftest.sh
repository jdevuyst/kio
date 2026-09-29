#!/bin/sh
# Verify compiler subprocess tests retain private executables through publication.
set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/lsp executable snapshot.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fake_repo=$scratch/repo
mkdir -p "$fake_repo/ci/checks/orchestrators/lib" \
  "$fake_repo/ci/checks/hygiene" "$fake_repo/kio-rs/src" \
  "$fake_repo/kio-rs/tests" "$fake_repo/docs" "$fake_repo/ci/infra"
cp "$REPO_ROOT/ci/checks/orchestrators/lsp-tests.sh" \
  "$fake_repo/ci/checks/orchestrators/"
cp "$REPO_ROOT/ci/checks/hygiene/kio-rs.sh" "$fake_repo/ci/checks/hygiene/"
cp "$REPO_ROOT/ci/checks/orchestrators/kiodoc-tests.sh" \
  "$fake_repo/ci/checks/orchestrators/kiodoc-tests.sh"
cp "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh" \
  "$fake_repo/ci/checks/orchestrators/lib/"

cat >"$fake_repo/ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  --prepare) printf '%s\n' "$0"; exit 0 ;;
  --readiness) [ "$2" = -- ]; exit 0 ;;
esac
[ "$1" = --resource ] && [ "$3" = -- ]
case "$2" in cargo|compiler) ;; *) exit 90 ;; esac
KIO_CI_SCHEDULE_HELD=$2
export KIO_CI_SCHEDULE_HELD
shift 3
exec "$@"
EOF

cat >"$fake_repo/ci/infra/sccache.sh" <<'EOF'
kio_configure_sccache_environment() { :; }
kio_ensure_sccache_ready() { :; }
EOF

cat >"$fake_repo/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  build)
    if [ "$#" -eq 1 ]; then
      mkdir -p target/debug
      printf '#!/bin/sh\nprintf "snapshot-cli\\n"\n' >target/debug/kio
      chmod +x target/debug/kio
      exit 0
    fi
    [ "$2" = --target-dir ]
    target=$3
    shift 3
    [ "$*" = '--all-features --bins' ] || {
      printf 'snapshot build did not request the full two-binary cohort\n' >&2
      exit 95
    }
    [ "${KIO_CI_SCHEDULE_HELD:-}" = cargo ]
    [ "$target" = "$PWD/target/kio-corpus-tools/kio-lsp-cli" ]
    mkdir -p "$target/debug"
    printf '#!/bin/sh\nprintf "snapshot-cli\\n"\n' >"$target/debug/kio"
    printf '#!/bin/sh\nprintf "snapshot-prime-cli\\n"\n' >"$target/debug/kio-prime"
    chmod +x "$target/debug/kio"
    chmod +x "$target/debug/kio-prime"
    ;;
  test)
    case "$*" in 'test --test lsp_smoke'|'test --all-features') ;; *) exit 90 ;; esac
    case "${KIO_DEBUG_TEST_KIO_BIN:-}" in
      "${ORCHESTRATOR_TMP:-}/kio") ;;
      *) printf 'test did not receive a private kio executable\n' >&2; exit 91 ;;
    esac
    [ -x "$KIO_DEBUG_TEST_KIO_BIN" ]
    [ "$("$KIO_DEBUG_TEST_KIO_BIN" --version)" = snapshot-cli ]
    if [ "$*" = 'test --all-features' ]; then
      [ "${KIO_DEBUG_TEST_KIO_PRIME_BIN:-}" = "$ORCHESTRATOR_TMP/kio-prime" ]
      [ "$("$KIO_DEBUG_TEST_KIO_PRIME_BIN" --version)" = snapshot-prime-cli ]
    fi
    # Republishing either Cargo output cannot change the already owned copy.
    mkdir -p target/debug
    printf '#!/bin/sh\nexit 92\n' >target/debug/kio
    printf '#!/bin/sh\nexit 93\n' >target/kio-corpus-tools/kio-lsp-cli/debug/kio
    printf '#!/bin/sh\nexit 92\n' >target/debug/kio-prime
    printf '#!/bin/sh\nexit 93\n' >target/kio-corpus-tools/kio-lsp-cli/debug/kio-prime
    [ "$("$KIO_DEBUG_TEST_KIO_BIN" --version)" = snapshot-cli ]
    printf '%s\n' "$KIO_DEBUG_TEST_KIO_BIN" >>"$KIO_TEST_SNAPSHOT_PATHS"
    if [ "$*" = 'test --all-features' ]; then
      [ "$("$KIO_DEBUG_TEST_KIO_PRIME_BIN" --version)" = snapshot-prime-cli ]
      printf '%s\n' "$KIO_DEBUG_TEST_KIO_PRIME_BIN" >>"$KIO_TEST_SNAPSHOT_PATHS"
    fi
    ;;
  fmt|clippy|check) ;;
  *) exit 94 ;;
esac
EOF

cat >"$fake_repo/ci/run-tests.sh" <<'EOF'
#!/bin/sh
set -eu
for arg in "$@"; do
  case $arg in
    --impl-def=*)
      compiler=${arg#*,kio=}
      compiler=${compiler%%,*}
      [ "$compiler" = "${ORCHESTRATOR_TMP:-}/kio" ] || {
        printf 'Kiodoc did not receive a private compiler\n' >&2
        exit 91
      }
      [ "$("$compiler")" = snapshot-cli ]
      mkdir -p kio-rs/target/debug
      printf '#!/bin/sh\nexit 92\n' >kio-rs/target/debug/kio
      printf '#!/bin/sh\nexit 93\n' >kio-rs/target/kio-corpus-tools/kio-lsp-cli/debug/kio
      [ "$("$compiler")" = snapshot-cli ]
      printf '%s\n' "$compiler" >>"$KIO_TEST_SNAPSHOT_PATHS"
      ;;
  esac
done
EOF

KIO_TEST_SNAPSHOT_PATHS=$scratch/snapshot-paths
export KIO_TEST_SNAPSHOT_PATHS
unset KIO_CI_SCHEDULE KIO_DEBUG_TEST_KIO_BIN KIO_DEBUG_TEST_KIO_PRIME_BIN
sh "$fake_repo/ci/checks/orchestrators/lsp-tests.sh"
sh "$fake_repo/ci/checks/hygiene/kio-rs.sh"
sh "$fake_repo/ci/checks/orchestrators/kiodoc-tests.sh" >/dev/null
[ "$(wc -l <"$KIO_TEST_SNAPSHOT_PATHS" | tr -d ' ')" = 4 ]
while IFS= read -r snapshot; do
  if [ -e "$snapshot" ]; then
    printf 'test caller leaked its completed snapshot: %s\n' "$snapshot" >&2
    exit 1
  fi
done <"$KIO_TEST_SNAPSHOT_PATHS"
printf 'lsp-executable-snapshot-selftest: compiler test callers preserve and clean private executables\n'
