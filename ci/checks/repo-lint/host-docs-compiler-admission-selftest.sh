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
[ "${1:-}" = build ]
case "${2:-}" in java|ts|python) ;; *) exit 2 ;; esac
case ",${KIO_CI_SCHEDULE_HELD:-}," in
  *,compiler,*) ;;
  *)
    printf 'kio build ran outside compiler admission\n' >&2
    exit 90
    ;;
esac
: >"$KIO_TEST_KIO_ADMITTED"
case "$2" in
  java)
    mkdir -p out/java/greeter
    printf '%s\n' 'package greeter; public class Greeter {}' >out/java/greeter/Greeter.java
    ;;
  ts)
    mkdir -p out/ts
    printf 'export {};\n' >out/ts/greeter.d.ts
    printf 'export {};\n' >out/ts/greeter.js
    ;;
  python)
    mkdir -p out/python/greeter
    printf 'class GreeterHost: ...\n' >out/python/greeter/__init__.pyi
    cat >out/python/greeter.py <<'PY'
import os
from types import SimpleNamespace

def create_greeter(host):
    def main():
        host.greeter.print(os.environ.get("KIO_TEST_GREETING", "hello from kio\n"))
    return SimpleNamespace(greeter=SimpleNamespace(KioModule_main=SimpleNamespace(main=main)))
PY
    ;;
esac
EOF
chmod +x "$scratch/fake-kio"

cat >"$fake_repo/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = build ] && [ "$2" = --target-dir ]
target_dir=$3
shift 3
expected='--all-features --bins'
expected="$expected --config profile.dev.package.kio-lang.opt-level=2"
expected="$expected --config profile.dev.package.kio-lang.debug-assertions=true"
expected="$expected --config profile.dev.package.kio-lang.overflow-checks=true"
[ "$*" = "$expected" ]
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

shell=$(command -v sh)
preflight_bin=$scratch/preflight-bin
mkdir "$preflight_bin"
ln -s "$(command -v dirname)" "$preflight_bin/dirname"
for page in js ts python rust; do
  if PATH="$preflight_bin" "$shell" "$fake_repo/ci/checks/orchestrators/host-docs-snippets.sh" "$page" >"$scratch/missing-$page.log" 2>&1; then
    printf 'host-docs-compiler-admission-selftest: absent required %s tool passed\n' "$page" >&2
    exit 1
  fi
  grep -q "FAIL $page: required host toolchain" "$scratch/missing-$page.log"
done
ln -s "$(command -v python3)" "$preflight_bin/python3"
if PATH="$preflight_bin" "$shell" "$fake_repo/ci/checks/orchestrators/host-docs-snippets.sh" python >"$scratch/missing-pyright.log" 2>&1; then
  printf 'host-docs-compiler-admission-selftest: absent required Pyright passed\n' >&2
  exit 1
fi
grep -q 'required typed-fence checker pyright' "$scratch/missing-pyright.log"

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

optional_bin=$scratch/optional-bin
mkdir "$optional_bin"
for tool in sh dirname cat cp mkdir mktemp rm awk grep sed find sort tr chmod head; do
  ln -s "$(command -v "$tool")" "$optional_bin/$tool"
done
PATH="$optional_bin" \
  KIO_CI_SCHEDULE_DIR="$state" \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_FAKE_KIO="$scratch/fake-kio" \
  KIO_TEST_KIO_ADMITTED="$scratch/optional-kio-admitted" \
  "$shell" "$fake_repo/ci/checks/orchestrators/host-docs-snippets.sh" java \
  >"$scratch/optional.log" 2>&1 || { cat "$scratch/optional.log" >&2; exit 1; }
grep -q 'host-fence compile SKIPPED' "$scratch/optional.log"
[ -e "$scratch/optional-kio-admitted" ]

cat >"$fake_repo/docs/hosts/ts.md" <<'EOF'
```kio {file}
package greeter;
```

```ts
export interface Greeter { missing: UndefinedType; }
export function createGreeter(): Greeter;
```
EOF
if KIO_CI_SCHEDULE_DIR="$state" \
  RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
  CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
  KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
  KIO_TEST_FAKE_KIO="$scratch/fake-kio" \
  KIO_TEST_KIO_ADMITTED="$scratch/ts-kio-admitted" \
  "$shell" "$fake_repo/ci/checks/orchestrators/host-docs-snippets.sh" ts \
  >"$scratch/declaration.log" 2>&1; then
  printf 'host-docs-compiler-admission-selftest: invalid declaration illustration passed\n' >&2
  exit 1
fi
grep -q "Cannot find name 'UndefinedType'" "$scratch/declaration.log" || {
  cat "$scratch/declaration.log" >&2; exit 1;
}
[ -e "$scratch/ts-kio-admitted" ]

# An always-green checker deliberately cannot detect the stub-only runtime
# import: only execution of the selected complete host can reject that defect.
mkdir "$scratch/python-bin"
printf '#!/bin/sh\nexit 0\n' >"$scratch/python-bin/pyright"
printf '#!/bin/sh\nexit 1\n' >"$scratch/python-bin/mise"
chmod +x "$scratch/python-bin/pyright" "$scratch/python-bin/mise"
cat >"$scratch/python-good.md" <<'EOF'
```kio {file}
package greeter;
```

```python
from greeter import create_greeter
host = unresolved_fragment_host
```

```python
from __future__ import annotations
import sys
from typing import TYPE_CHECKING
from greeter import create_greeter
if TYPE_CHECKING:
    from greeter import GreeterHost
class GreeterCapabilities:
    def print(self, p0: str) -> None:
        sys.stdout.write(p0)
class Host:
    greeter = GreeterCapabilities()
def run(host: GreeterHost[str]) -> None:
    create_greeter(host).greeter.KioModule_main.main()
run(Host())
```
EOF
run_python_fixture() {
  PATH="$scratch/python-bin:$PATH" \
    KIO_CI_SCHEDULE_DIR="$state" \
    RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' \
    CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
    KIO_TEST_RUNNER_COMPILER_WRAPPER='' \
    KIO_TEST_FAKE_KIO="$scratch/fake-kio" \
    KIO_TEST_KIO_ADMITTED="$scratch/python-kio-admitted" \
    "$shell" "$fake_repo/ci/checks/orchestrators/host-docs-snippets.sh" python
}
cp "$scratch/python-good.md" "$fake_repo/docs/hosts/python.md"
run_python_fixture >"$scratch/python-good.log" 2>&1 || {
  cat "$scratch/python-good.log" >&2; exit 1;
}
grep -q 'typed host executed with output assertion' "$scratch/python-good.log"
[ -e "$scratch/python-kio-admitted" ]

sed 's/from greeter import create_greeter/from greeter import GreeterHost, create_greeter/' \
  "$scratch/python-good.md" >"$fake_repo/docs/hosts/python.md"
if run_python_fixture >"$scratch/python-import.log" 2>&1; then
  printf 'host-docs-compiler-admission-selftest: stub-only runtime import passed\n' >&2
  exit 1
fi
grep -q 'ImportError' "$scratch/python-import.log" || {
  cat "$scratch/python-import.log" >&2; exit 1;
}

cp "$scratch/python-good.md" "$fake_repo/docs/hosts/python.md"
if KIO_TEST_GREETING='wrong greeting' run_python_fixture >"$scratch/python-output.log" 2>&1; then
  printf 'host-docs-compiler-admission-selftest: wrong typed-host output passed\n' >&2
  exit 1
fi
grep -q 'typed host must print hello from kio' "$scratch/python-output.log"

sed '/^run(Host())$/d' "$scratch/python-good.md" >"$fake_repo/docs/hosts/python.md"
if run_python_fixture >"$scratch/python-selection.log" 2>&1; then
  printf 'host-docs-compiler-admission-selftest: unselected typed host passed\n' >&2
  exit 1
fi
grep -q 'expected one executable typed host, found 0' "$scratch/python-selection.log"

printf 'host-docs-compiler-admission-selftest: ok\n'
