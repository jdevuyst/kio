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
cp "$REPO_ROOT/ci/package-kio.sh" "$fake_repo/ci/"
git -C "$fake_repo" init -q
git -C "$fake_repo" -c user.name=Fixture -c user.email=fixture@example.invalid \
  commit --allow-empty -qm fixture
mkdir -p "$fake_repo/kio-rs/src/backends/js"
cp "$REPO_ROOT/README.md" "$fake_repo/README.md"
cp "$fake_repo/README.md" "$fake_repo/kio-rs/README.md"
git -C "$fake_repo" add -- README.md kio-rs/README.md
git -C "$fake_repo" -c user.name=Fixture -c user.email=fixture@example.invalid commit -qm readme
cp "$REPO_ROOT/ci/infra/check-readme-examples.sh" "$fake_repo/ci/infra/"
cp "$REPO_ROOT/kio-rs/src/backends/js/value_runtime.js" \
  "$REPO_ROOT/kio-rs/src/backends/js/value_runtime.test.mjs" \
  "$fake_repo/kio-rs/src/backends/js/"
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
    expected='--all-features --bins'
    expected="$expected --config profile.dev.package.kio-lang.opt-level=2"
    expected="$expected --config profile.dev.package.kio-lang.debug-assertions=true"
    expected="$expected --config profile.dev.package.kio-lang.overflow-checks=true"
    [ "$*" = "$expected" ] || {
      printf 'snapshot build changed the full cohort or compiler profile\n' >&2
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
    hygiene_test='test --all-features --config profile.test.package.kio-lang.opt-level=2'
    hygiene_test="$hygiene_test --config profile.test.debug-assertions=true"
    hygiene_test="$hygiene_test --config profile.test.overflow-checks=true"
    case "$*" in
      'test --test lsp_smoke'|"$hygiene_test") ;;
      *) printf 'test caller changed its expected Cargo profile or arguments: %s\n' "$*" >&2; exit 90 ;;
    esac
    case "${KIO_DEBUG_TEST_KIO_BIN:-}" in
      "${ORCHESTRATOR_TMP:-}/kio") ;;
      *) printf 'test did not receive a private kio executable\n' >&2; exit 91 ;;
    esac
    [ -x "$KIO_DEBUG_TEST_KIO_BIN" ]
    [ "$("$KIO_DEBUG_TEST_KIO_BIN" --version)" = snapshot-cli ]
    if [ "$*" = "$hygiene_test" ]; then
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
    if [ "$*" = "$hygiene_test" ]; then
      [ "$("$KIO_DEBUG_TEST_KIO_PRIME_BIN" --version)" = snapshot-prime-cli ]
      printf '%s\n' "$KIO_DEBUG_TEST_KIO_PRIME_BIN" >>"$KIO_TEST_SNAPSHOT_PATHS"
    fi
    ;;
  run)
    case "$*" in
      'run --locked --example pack_manual -- --write') ;;
      "run --example pack_manual -- --check-package $PWD/target/package/kio-lang-0.crate") ;;
      *) exit 90 ;;
    esac
    ;;
  package)
    case "$*" in
      'package --list --allow-dirty --locked')
        printf '%s\n' Cargo.toml.orig .cargo_vcs_info.json manual/index.tsv manual/payload.tar.gz-concat README.md
        ;;
      'package --locked --no-verify --allow-dirty') ;;
      *) exit 90 ;;
    esac
    ;;
  metadata)
    [ "$*" = 'metadata --format-version 1 --no-deps' ] || exit 90
    printf '{"target_directory":"%s/target","packages":[{"name":"kio-lang","version":"0"}]}' "$PWD"
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

mkdir -p "$fake_repo/website" "$scratch/bin"
cp "$REPO_ROOT/ci/checks/orchestrators/website-e2e.sh" \
  "$fake_repo/ci/checks/orchestrators/"
for tool in node mise wasm-pack cargo; do
  printf '#!/bin/sh\nexit 0\n' >"$scratch/bin/$tool"
  chmod +x "$scratch/bin/$tool"
done
cat >"$scratch/bin/npm" <<'EOF'
#!/bin/sh
set -eu
case "$*" in
  'ci --no-fund --no-audit'|'run check:examples'|'run build'|'run audit'|\
  'run smoke'|'run browser:smoke'|'run browser:history') ;;
  *) exit 90 ;;
esac
[ "$KIO_BIN" = "$ORCHESTRATOR_TMP/kio" ]
[ "$("$KIO_BIN")" = snapshot-cli ]
printf '#!/bin/sh\nexit 93\n' >../kio-rs/target/kio-corpus-tools/kio-lsp-cli/debug/kio
[ "$("$KIO_BIN")" = snapshot-cli ]
printf '%s\n' "$KIO_BIN" >>"$KIO_TEST_SNAPSHOT_PATHS"
if [ "$*" = "${KIO_TEST_WEBSITE_FAIL:-}" ]; then exit 17; fi
EOF
chmod +x "$scratch/bin/npm"
PATH="$scratch/bin:$PATH" sh "$fake_repo/ci/checks/orchestrators/website-e2e.sh"
[ "$(wc -l <"$KIO_TEST_SNAPSHOT_PATHS" | tr -d ' ')" = 11 ]
website_status=0
PATH="$scratch/bin:$PATH" KIO_TEST_WEBSITE_FAIL='run smoke' \
  sh "$fake_repo/ci/checks/orchestrators/website-e2e.sh" || website_status=$?
[ "$website_status" = 17 ]
[ "$(wc -l <"$KIO_TEST_SNAPSHOT_PATHS" | tr -d ' ')" = 16 ]

mkdir -p "$fake_repo/website/scripts" "$fake_repo/website/examples/hello-world"
cp "$REPO_ROOT/website/scripts/check-examples.sh" "$fake_repo/website/scripts/"
cp "$REPO_ROOT/website/examples/hello-world/hello.kio" \
  "$REPO_ROOT/website/examples/hello-world/hello.pkg.kio" \
  "$fake_repo/website/examples/hello-world/"
cat >"$scratch/admitted-kio" <<'EOF'
#!/bin/sh
set -eu
[ "${KIO_CI_SCHEDULE_HELD:-}" = compiler ] || exit 94
case "$1" in check|fmt) ;; *) exit 90 ;; esac
EOF
chmod +x "$scratch/admitted-kio"
KIO_BIN="$scratch/admitted-kio" sh "$fake_repo/website/scripts/check-examples.sh"
while IFS= read -r snapshot; do
  if [ -e "$snapshot" ]; then
    printf 'test caller leaked its completed snapshot: %s\n' "$snapshot" >&2
    exit 1
  fi
done <"$KIO_TEST_SNAPSHOT_PATHS"
printf 'lsp-executable-snapshot-selftest: compiler test callers preserve and clean private executables\n'
