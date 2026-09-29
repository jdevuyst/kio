#!/bin/sh
#
# Verify that host-documentation native compilers enter shared admission.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
if [ -z "${KIO_CI_SCHEDULER_BIN:-}" ]; then
  KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare)
  export KIO_CI_SCHEDULER_BIN
fi

scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/host-docs-compiler-admission.XXXXXX") || {
  printf 'host-docs-compiler-admission-selftest: cannot make scratch directory\n' >&2
  exit 2
}
pids=
cleanup() {
  trap - EXIT INT TERM HUP
  for pid in $pids; do
    kill -TERM "$pid" >/dev/null 2>&1 || :
  done
  for pid in $pids; do
    wait "$pid" >/dev/null 2>&1 || :
  done
  rm -rf "$scratch"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

fake_repo=$scratch/repo
mkdir -p \
  "$fake_repo/ci/checks/orchestrators/lib" \
  "$fake_repo/ci/infra" \
  "$fake_repo/docs/hosts" \
  "$fake_repo/kio-rs" \
  "$scratch/bin"
cp "$REPO_ROOT/ci/checks/orchestrators/host-docs-snippets.sh" \
  "$fake_repo/ci/checks/orchestrators/"
cp "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh" \
  "$fake_repo/ci/checks/orchestrators/lib/"
cp "$REPO_ROOT/ci/schedule.sh" "$fake_repo/ci/"
cp "$REPO_ROOT/ci/infra/sccache.sh" "$fake_repo/ci/infra/"

cat >"$scratch/fake-kio" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = build ] && [ "${2:-}" = java ]
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *)
    printf 'kio build ran outside compiler admission\n' >&2
    exit 90
    ;;
esac
: >"$KIO_TEST_KIO_ADMITTED"
mkdir -p out/java/greeter
printf '%s\n' 'package greeter; public class Greeter {}' \
  >out/java/greeter/Greeter.java
EOF
chmod +x "$scratch/fake-kio"

cat >"$fake_repo/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = build ] && [ "$2" = --target-dir ]
target_dir=$3
shift 3
[ "$*" = '--all-features --bins' ]
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,cargo,*) ;;
  *) exit 92 ;;
esac
mkdir -p "$target_dir/debug"
cp "$KIO_TEST_FAKE_KIO" "$target_dir/debug/kio"
chmod +x "$target_dir/debug/kio"
EOF

cat >"$fake_repo/docs/hosts/java.md" <<'EOF'
```kio {file}
package greeter;
```

```java
public class Main {
    public static void main(String[] args) {}
}
```
EOF

cat >"$scratch/bin/javac" <<'EOF'
#!/bin/sh
set -eu
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *)
    printf 'javac ran outside compiler admission\n' >&2
    exit 91
    ;;
esac
: >"$KIO_TEST_ADMITTED"
EOF
chmod +x "$scratch/bin/javac"

cat >"$scratch/hold" <<'EOF'
#!/bin/sh
set -eu
: >"$KIO_TEST_READY"
while [ ! -e "$KIO_TEST_RELEASE" ]; do
  sleep 0.01
done
EOF
chmod +x "$scratch/hold"

wait_for_file() {
  wff_file=$1
  wff_attempt=0
  while [ ! -e "$wff_file" ]; do
    wff_attempt=$((wff_attempt + 1))
    [ "$wff_attempt" -lt 500 ] || return 1
    sleep 0.01
  done
}

wait_for_pending() {
  wfp_dir=$1
  wfp_attempt=0
  while ! has_pending_claim "$wfp_dir"; do
    wfp_attempt=$((wfp_attempt + 1))
    [ "$wfp_attempt" -lt 500 ] || return 1
    sleep 0.01
  done
}

has_pending_claim() {
  hpc_dir=$1
  for hpc_claim in "$hpc_dir"/*.pending; do
    [ -f "$hpc_claim" ] && return 0
  done
  return 1
}

state=$scratch/state
ready=$scratch/holder-ready
release=$scratch/holder-release
KIO_CI_SCHEDULE_DIR="$state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_READY="$ready" \
  KIO_TEST_RELEASE="$release" \
  sh "$fake_repo/ci/schedule.sh" --resource compiler -- "$scratch/hold" &
holder_pid=$!
pids="$pids $holder_pid"
wait_for_file "$ready" || {
  printf 'host-docs-compiler-admission-selftest: holder was not admitted\n' >&2
  exit 1
}

log=$scratch/run.log
PATH="$scratch/bin:$PATH" \
  KIO_CI_SCHEDULE_DIR="$state" \
  KIO_CI_SCHEDULE_HELD='' \
  KIO_CI_SCHEDULE_COMPILER_JOBS=1 \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_FAKE_KIO="$scratch/fake-kio" \
  KIO_TEST_KIO_ADMITTED="$scratch/kio-admitted" \
  KIO_TEST_ADMITTED="$scratch/native-admitted" \
  sh "$fake_repo/ci/checks/orchestrators/host-docs-snippets.sh" java \
    >"$log" 2>&1 &
host_docs_pid=$!
pids="$pids $host_docs_pid"

wait_for_pending "$state/compiler/claims" || {
  cat "$log" >&2
  printf 'host-docs-compiler-admission-selftest: compiler-producing command did not wait for the occupied permit\n' >&2
  exit 1
}
if [ -e "$scratch/kio-admitted" ] || [ -e "$scratch/native-admitted" ]; then
  printf 'host-docs-compiler-admission-selftest: compiler exceeded capacity one\n' >&2
  exit 1
fi

: >"$release"
if ! wait "$holder_pid"; then
  printf 'host-docs-compiler-admission-selftest: holder failed\n' >&2
  exit 1
fi
if ! wait "$host_docs_pid"; then
  cat "$log" >&2
  printf 'host-docs-compiler-admission-selftest: admitted host compiler failed\n' >&2
  exit 1
fi
pids=

[ -e "$scratch/kio-admitted" ] || {
  printf 'host-docs-compiler-admission-selftest: kio build did not enter the released slot\n' >&2
  exit 1
}
[ -e "$scratch/native-admitted" ] || {
  printf 'host-docs-compiler-admission-selftest: host compiler did not enter the released slot\n' >&2
  exit 1
}
if find "$state/compiler/claims" -type f -print -quit | grep -q .; then
  printf 'host-docs-compiler-admission-selftest: host compiler left a lease behind\n' >&2
  exit 1
fi

printf 'host-docs-compiler-admission-selftest: ok\n'
