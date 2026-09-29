#!/bin/sh
#
# Verify that run-tests admits semantic Kio commands without serializing
# cheap CLI-management commands.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/run-tests-kio-admission.XXXXXX") || {
  printf 'run-tests-kio-compiler-admission-selftest: cannot make scratch directory\n' >&2
  exit 2
}
cleanup() {
  rm -rf "$scratch"
}
trap cleanup EXIT INT TERM HUP

mkdir -p \
  "$scratch/bin with space" \
  "$scratch/cache" \
  "$scratch/ambient" \
  "$scratch/no-prime-sibling" \
  "$scratch/cases/00_standard/workdir" \
  "$scratch/cases/01_custom" \
  "$scratch/cases/02_prime_identity" \
  "$scratch/cases/03_reserved_identity" \
  "$scratch/cases/04_missing_prime" \
  "$scratch/tmp"

cat >"$scratch/bin with space/kio-tool" <<'EOF'
#!/bin/sh
set -eu

subcommand=
for arg in "$@"; do
  [ "$arg" = --no-cache ] && continue
  subcommand=$arg
  break
done

case "$subcommand" in
  ''|-h|--help|-V|--version|fmt|cache|dep|init|completions)
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*)
        printf '%s %s was needlessly admitted\n' "${0##*/}" "$subcommand" >&2
        exit 91
        ;;
    esac
    printf '%s\t%s\n' "${0##*/}" "${subcommand:-information}" \
      >>"$KIO_TEST_KIO_DIRECT"
    ;;
  *)
    case ",${KIO_CI_SCHEDULE_HELD:-}," in
      *,compiler,*) ;;
      *)
        printf '%s %s ran outside compiler admission\n' "${0##*/}" "$subcommand" >&2
        exit 92
        ;;
    esac
    printf '%s\t%s\n' "${0##*/}" "$subcommand" \
      >>"$KIO_TEST_KIO_ADMITTED"
    ;;
esac

case "$subcommand" in
  build)
    target=${2:-js}
    mkdir -p "out/$target"
    ;;
esac
EOF
chmod +x "$scratch/bin with space/kio-tool"
ln -s kio-tool "$scratch/bin with space/kio"
ln -s kio-tool "$scratch/bin with space/kio-prime"
ln -s kio-tool "$scratch/bin with space/real"
ln -s kio-tool "$scratch/bin with space/admission-path"
ln -s "$scratch/bin with space/kio-tool" "$scratch/no-prime-sibling/kio"

cat >"$scratch/ambient/kio-prime" <<'EOF'
#!/bin/sh
set -eu
: >"$KIO_TEST_AMBIENT_PRIME_RAN"
exit 0
EOF
chmod +x "$scratch/ambient/kio-prime"

cat >"$scratch/bin with space/runner" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$scratch/bin with space/runner"

cat >"$scratch/bin with space/sccache" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 1 ] && [ "$1" = --dist-status ]
[ "${SCCACHE_IDLE_TIMEOUT:-}" = 0 ]
[ -z "${KIO_CI_SCHEDULE_HELD:-}" ]
[ -z "${KIO_CI_SCHEDULE_LEASE_FDS:-}" ]
printf '%s\n' "$1" >>"$KIO_TEST_SCCACHE_LOG"
EOF
chmod +x "$scratch/bin with space/sccache"

cat >"$scratch/cases/00_standard/workdir/fixture.pkg.kio" <<'EOF'
package fixture;

build {
  target js {
    out "out/js/";
  }
}
EOF
: >"$scratch/cases/00_standard/run.args"

cat >"$scratch/cases/01_custom/run.sh" <<'EOF'
#!/bin/sh
set -eu
"$KIO_BIN" --no-cache fmt
kio dep fetch
"$KIO_BIN" check
kio future-command
kio-prime build js
EOF
chmod +x "$scratch/cases/01_custom/run.sh"

cat >"$scratch/cases/02_prime_identity/run.sh" <<'EOF'
#!/bin/sh
set -eu
[ "${KIO_BIN##*/}" = kio-prime ] || {
  printf 'configured kio-prime identity became %s\n' "${KIO_BIN##*/}" >&2
  exit 93
}
"$KIO_BIN" check
: >"$KIO_TEST_PRIME_CASE_IDENTITY"
EOF
chmod +x "$scratch/cases/02_prime_identity/run.sh"
: >"$scratch/cases/02_prime_identity/IS_KIO_PRIME"

cat >"$scratch/cases/03_reserved_identity/run.sh" <<'EOF'
#!/bin/sh
set -eu
case "${KIO_BIN##*/}" in
  real) : >"$KIO_TEST_RESERVED_REAL_CASE" ;;
  admission-path) : >"$KIO_TEST_RESERVED_ADMISSION_CASE" ;;
  *)
    printf 'reserved-looking compiler identity became %s\n' \
      "${KIO_BIN##*/}" >&2
    exit 95
    ;;
esac
"$KIO_BIN" check
EOF
chmod +x "$scratch/cases/03_reserved_identity/run.sh"

cat >"$scratch/cases/04_missing_prime/run.sh" <<'EOF'
#!/bin/sh
set -eu
if kio-prime check >"$KIO_TEST_MISSING_PRIME_LOG" 2>&1; then
  printf 'unconfigured ambient kio-prime was executed\n' >&2
  exit 97
fi
grep -q 'no resolved kio-prime command' "$KIO_TEST_MISSING_PRIME_LOG" || {
  cat "$KIO_TEST_MISSING_PRIME_LOG" >&2
  printf 'missing companion did not fail through the Kio proxy\n' >&2
  exit 98
}
[ ! -e "$KIO_TEST_AMBIENT_PRIME_RAN" ] || {
  printf 'ambient kio-prime left an execution witness\n' >&2
  exit 99
}
"$KIO_BIN" check
: >"$KIO_TEST_MISSING_PRIME_CASE"
EOF
chmod +x "$scratch/cases/04_missing_prime/run.sh"

for case_dir in \
  "$scratch/cases/00_standard" \
  "$scratch/cases/01_custom" \
  "$scratch/cases/02_prime_identity" \
  "$scratch/cases/03_reserved_identity" \
  "$scratch/cases/04_missing_prime"
do
  printf '0\n' >"$case_dir/expected.exit"
  : >"$case_dir/expected.stdout"
  : >"$case_dir/expected.stderr.ignore"
done

cat >"$scratch/impl-check.sh" <<'EOF'
#!/bin/sh
set -eu
"$KIO_BIN" fmt
"$KIO_BIN" test
"$KIO_PRIME_BIN" build js
EOF
chmod +x "$scratch/impl-check.sh"

cat >"$scratch/case-binary-check.sh" <<'EOF'
#!/bin/sh
# ROUTING: case-binary
set -eu
"$KIO_BIN" check
: >"$KIO_TEST_CASE_BINARY_CHECK"
EOF
chmod +x "$scratch/case-binary-check.sh"

cat >"$scratch/prime-case-binary-check.sh" <<'EOF'
#!/bin/sh
# ROUTING: case-binary
set -eu
[ "${KIO_BIN##*/}" = kio-prime ] || {
  printf 'case-binary kio-prime identity became %s\n' "${KIO_BIN##*/}" >&2
  exit 94
}
"$KIO_BIN" check
: >"$KIO_TEST_PRIME_BINARY_IDENTITY"
EOF
chmod +x "$scratch/prime-case-binary-check.sh"

cat >"$scratch/reserved-case-binary-check.sh" <<'EOF'
#!/bin/sh
# ROUTING: case-binary
set -eu
case "${KIO_BIN##*/}" in
  real) : >"$KIO_TEST_RESERVED_REAL_BINARY" ;;
  admission-path) : >"$KIO_TEST_RESERVED_ADMISSION_BINARY" ;;
  *)
    printf 'case-binary reserved-looking identity became %s\n' \
      "${KIO_BIN##*/}" >&2
    exit 96
    ;;
esac
"$KIO_BIN" check
EOF
chmod +x "$scratch/reserved-case-binary-check.sh"

log=$scratch/run.log
PATH="$scratch/bin with space:$PATH" \
  KIO_CI_SCHEDULE_DIR="$scratch/state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_JOBS=2 \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_KIO_DIRECT="$scratch/kio-direct" \
  KIO_TEST_KIO_ADMITTED="$scratch/kio-admitted" \
  KIO_TEST_CASE_BINARY_CHECK="$scratch/case-binary-check-ran" \
  TMPDIR="$scratch/tmp" \
  sh "$REPO_ROOT/ci/run-tests.sh" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def="name=fixture,kio=$scratch/bin with space/kio,runner=$scratch/bin with space/runner,target=js,prime-kio=$scratch/bin with space/kio-prime" \
    --check="$scratch/impl-check.sh" \
    --check="$scratch/case-binary-check.sh" \
    --jobs=2 \
    --compiler-jobs=1 \
    '^0[01]_' >"$log" 2>&1 || {
      cat "$log" >&2
      printf 'run-tests-kio-compiler-admission-selftest: harness failed\n' >&2
      exit 1
    }

TAB=$(printf '\t')
for command in test build check future-command; do
  grep -q "$TAB$command\$" "$scratch/kio-admitted" || {
    cat "$scratch/kio-admitted" >&2
    printf 'run-tests-kio-compiler-admission-selftest: missing admitted %s command\n' \
      "$command" >&2
    exit 1
  }
done
for command in cache fmt dep; do
  grep -q "$TAB$command\$" "$scratch/kio-direct" || {
    cat "$scratch/kio-direct" >&2
    printf 'run-tests-kio-compiler-admission-selftest: missing direct %s command\n' \
      "$command" >&2
    exit 1
  }
done
grep -q '^kio-prime	build$' "$scratch/kio-admitted" || {
  cat "$scratch/kio-admitted" >&2
  printf 'run-tests-kio-compiler-admission-selftest: sibling kio-prime was not admitted\n' >&2
  exit 1
}
[ -e "$scratch/case-binary-check-ran" ] || {
  printf 'run-tests-kio-compiler-admission-selftest: case-binary check did not run\n' >&2
  exit 1
}
if find "$scratch/state/compiler/claims" -type f -print -quit | grep -q .; then
  printf 'run-tests-kio-compiler-admission-selftest: Kio command left a lease behind\n' >&2
  exit 1
fi

prime_log=$scratch/prime-run.log
PATH="$scratch/bin with space:$PATH" \
  KIO_CI_SCHEDULE_DIR="$scratch/prime-state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_JOBS=1 \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_KIO_DIRECT="$scratch/prime-kio-direct" \
  KIO_TEST_KIO_ADMITTED="$scratch/prime-kio-admitted" \
  KIO_TEST_PRIME_CASE_IDENTITY="$scratch/prime-case-identity-ran" \
  KIO_TEST_PRIME_BINARY_IDENTITY="$scratch/prime-binary-identity-ran" \
  TMPDIR="$scratch/tmp" \
  sh "$REPO_ROOT/ci/run-tests.sh" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def="name=prime,kio=$scratch/bin with space/kio-prime,runner=$scratch/bin with space/runner,target=js" \
    --check="$scratch/prime-case-binary-check.sh" \
    --jobs=1 \
    --compiler-jobs=1 \
    --prime-only \
    '^02_prime_identity$' >"$prime_log" 2>&1 || {
      cat "$prime_log" >&2
      printf 'run-tests-kio-compiler-admission-selftest: prime identity harness failed\n' >&2
      exit 1
    }

for marker in prime-case-identity-ran prime-binary-identity-ran; do
  [ -e "$scratch/$marker" ] || {
    printf 'run-tests-kio-compiler-admission-selftest: missing %s witness\n' \
      "$marker" >&2
    exit 1
  }
done
grep -q '^kio-prime	check$' "$scratch/prime-kio-admitted" || {
  cat "$scratch/prime-kio-admitted" >&2
  printf 'run-tests-kio-compiler-admission-selftest: prime check was not admitted\n' >&2
  exit 1
}

reserved_log=$scratch/reserved-run.log
: >"$scratch/reserved-sccache.log"
PATH="$scratch/bin with space:$PATH" \
  KIO_CI_SCHEDULE_DIR="$scratch/reserved-state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_JOBS=2 \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER="$scratch/bin with space/sccache" \
  KIO_TEST_SCCACHE_LOG="$scratch/reserved-sccache.log" \
  KIO_TEST_KIO_DIRECT="$scratch/reserved-kio-direct" \
  KIO_TEST_KIO_ADMITTED="$scratch/reserved-kio-admitted" \
  KIO_TEST_RESERVED_REAL_CASE="$scratch/reserved-real-case-ran" \
  KIO_TEST_RESERVED_ADMISSION_CASE="$scratch/reserved-admission-case-ran" \
  KIO_TEST_RESERVED_REAL_BINARY="$scratch/reserved-real-binary-ran" \
  KIO_TEST_RESERVED_ADMISSION_BINARY="$scratch/reserved-admission-binary-ran" \
  TMPDIR="$scratch/tmp" \
  sh "$REPO_ROOT/ci/run-tests.sh" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def="name=reserved-admission,kio=$scratch/bin with space/admission-path,runner=$scratch/bin with space/runner,target=js" \
    --impl-def="name=reserved-real,kio=$scratch/bin with space/real,runner=$scratch/bin with space/runner,target=js" \
    --check="$scratch/reserved-case-binary-check.sh" \
    --jobs=2 \
    --compiler-jobs=1 \
    '^03_reserved_identity$' >"$reserved_log" 2>&1 || {
      cat "$reserved_log" >&2
      printf 'run-tests-kio-compiler-admission-selftest: reserved identity harness failed\n' >&2
      exit 1
    }

for marker in \
  reserved-real-case-ran \
  reserved-admission-case-ran \
  reserved-real-binary-ran \
  reserved-admission-binary-ran
do
  [ -e "$scratch/$marker" ] || {
    printf 'run-tests-kio-compiler-admission-selftest: missing %s witness\n' \
      "$marker" >&2
    exit 1
  }
done
if [ "$(wc -l <"$scratch/reserved-sccache.log" | tr -d ' ')" -ne 1 ]; then
  cat "$scratch/reserved-sccache.log" >&2
  printf 'run-tests-kio-compiler-admission-selftest: renamed semantic Kio commands repeated runner-wrapper readiness\n' >&2
  exit 1
fi
for compiler in real admission-path; do
  grep -q "^$compiler	check$" "$scratch/reserved-kio-admitted" || {
    cat "$scratch/reserved-kio-admitted" >&2
    printf 'run-tests-kio-compiler-admission-selftest: %s check was not admitted\n' \
      "$compiler" >&2
    exit 1
  }
done

missing_log=$scratch/missing-prime-run.log
PATH="$scratch/ambient:$PATH" \
  KIO_CI_SCHEDULE_DIR="$scratch/missing-prime-state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_JOBS=1 \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_KIO_DIRECT="$scratch/missing-prime-kio-direct" \
  KIO_TEST_KIO_ADMITTED="$scratch/missing-prime-kio-admitted" \
  KIO_TEST_AMBIENT_PRIME_RAN="$scratch/ambient-prime-ran" \
  KIO_TEST_MISSING_PRIME_LOG="$scratch/missing-prime.log" \
  KIO_TEST_MISSING_PRIME_CASE="$scratch/missing-prime-case-ran" \
  TMPDIR="$scratch/tmp" \
  sh "$REPO_ROOT/ci/run-tests.sh" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def="name=missing-prime,kio=$scratch/no-prime-sibling/kio,runner=$scratch/bin with space/runner,target=js" \
    --jobs=1 \
    --compiler-jobs=1 \
    '^04_missing_prime$' >"$missing_log" 2>&1 || {
      cat "$missing_log" >&2
      printf 'run-tests-kio-compiler-admission-selftest: missing prime harness failed\n' >&2
      exit 1
    }

[ -e "$scratch/missing-prime-case-ran" ] || {
  printf 'run-tests-kio-compiler-admission-selftest: missing prime case did not run\n' >&2
  exit 1
}
[ ! -e "$scratch/ambient-prime-ran" ] || {
  printf 'run-tests-kio-compiler-admission-selftest: ambient kio-prime ran\n' >&2
  exit 1
}
grep -q '^kio	check$' "$scratch/missing-prime-kio-admitted" || {
  cat "$scratch/missing-prime-kio-admitted" >&2
  printf 'run-tests-kio-compiler-admission-selftest: configured kio was not admitted\n' >&2
  exit 1
}

# Git Bash's default symlink emulation copies targets, and a bare name can
# alias an existing .exe. Exercise that setup without copying real compilers.
mkdir -p "$scratch/msys-tools" "$scratch/exe tools"
cp "$scratch/bin with space/kio-tool" "$scratch/exe tools/kio.exe"
cp "$scratch/bin with space/kio-tool" "$scratch/exe tools/kio-prime.exe"
cat >"$scratch/msys-tools/ln" <<'EOF'
#!/bin/sh
set -eu
if [ "$#" -eq 3 ] && [ "$1" = -s ]; then
  case "$3" in
    */kio-compiler-proxy/*)
      if [ -e "$3" ] || [ -e "$3.exe" ]; then
        printf 'emulated MSYS ln: %s: File exists\n' "$3" >&2
        exit 1
      fi
      cp "$2" "$3"
      exit
      ;;
  esac
fi
exec "$KIO_TEST_REAL_LN" "$@"
EOF
cat >"$scratch/msys-tools/cp" <<'EOF'
#!/bin/sh
set -eu
if [ "$#" -eq 2 ]; then
  case "$1" in
    "$KIO_TEST_EXE"|"$KIO_TEST_PRIME_EXE")
      printf '%s\t%s\n' "$1" "$2" >>"$KIO_TEST_EXE_COPIES"
      ;;
  esac
fi
exec "$KIO_TEST_REAL_CP" "$@"
EOF
chmod +x "$scratch/msys-tools/ln" "$scratch/msys-tools/cp"
exe_log=$scratch/exe-run.log
exe_real_ln=$(command -v ln)
exe_real_cp=$(command -v cp)
PATH="$scratch/msys-tools:$scratch/bin with space:$PATH" \
  KIO_TEST_REAL_LN="$exe_real_ln" \
  KIO_TEST_REAL_CP="$exe_real_cp" \
  KIO_TEST_EXE="$scratch/exe tools/kio.exe" \
  KIO_TEST_PRIME_EXE="$scratch/exe tools/kio-prime.exe" \
  KIO_TEST_EXE_COPIES="$scratch/exe-copies" \
  KIO_CI_SCHEDULE_DIR="$scratch/exe-state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_JOBS=2 \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_KIO_DIRECT="$scratch/exe-direct" \
  KIO_TEST_KIO_ADMITTED="$scratch/exe-admitted" \
  TMPDIR="$scratch/tmp" \
  sh "$REPO_ROOT/ci/run-tests.sh" \
    --cases-dir="$scratch/cases" \
    --cache-base="$scratch/cache" \
    --impl-def="name=exe,kio=$scratch/exe tools/kio.exe,runner=$scratch/bin with space/runner,target=js,prime-kio=$scratch/exe tools/kio-prime.exe" \
    --jobs=2 \
    --compiler-jobs=1 \
    '^00_standard$' '^01_custom$' >"$exe_log" 2>&1 || {
      cat "$exe_log" >&2
      if [ -e "$scratch/exe-copies" ]; then
        cat "$scratch/exe-copies" >&2
      fi
      printf 'run-tests-kio-compiler-admission-selftest: executable alias harness failed\n' >&2
      exit 1
    }
[ ! -e "$scratch/exe-copies" ] || {
  cat "$scratch/exe-copies" >&2
  printf 'run-tests-kio-compiler-admission-selftest: proxy copied compiler bytes\n' >&2
  exit 1
}
for command in test build check future-command; do
  grep -q "^kio.exe$TAB$command\$" "$scratch/exe-admitted" || {
    cat "$scratch/exe-admitted" >&2
    printf 'run-tests-kio-compiler-admission-selftest: missing admitted exe %s\n' \
      "$command" >&2
    exit 1
  }
done
grep -q '^kio-prime.exe	build$' "$scratch/exe-admitted" || {
  cat "$scratch/exe-admitted" >&2
  printf 'run-tests-kio-compiler-admission-selftest: explicit exe companion was not admitted\n' >&2
  exit 1
}

printf 'run-tests-kio-compiler-admission-selftest: ok\n'
