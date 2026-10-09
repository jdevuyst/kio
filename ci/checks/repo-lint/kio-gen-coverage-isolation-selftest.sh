#!/bin/sh
# Exercise coverage isolation and evidence rejection without compiling.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
scratch_parent=${TMPDIR:-"$REPO_ROOT/target"}
mkdir -p "$scratch_parent"
scratch=$(mktemp -d "$scratch_parent/coverage-isolation.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
fake=$scratch/repo
mkdir -p "$fake/reports" "$fake/ci/checks/orchestrators/lib" "$fake/kio-rs" \
  "$fake/ci/infra/kio-gen-rs" "$fake/ci/infra/kio-prime-check-rs" "$fake/ci/infra/kio-test-runner-rs"
cp "$REPO_ROOT/reports/kio-gen-coverage-delta.sh" "$fake/reports/"
cp "$REPO_ROOT/ci/checks/orchestrators/lib/common.sh" "$fake/ci/checks/orchestrators/lib/"
mkdir "$scratch/ordinary-target"
printf 'ordinary target sentinel\n' >"$scratch/sentinel"
cp "$scratch/sentinel" "$scratch/ordinary-target/sentinel"
ln -s "$scratch/ordinary-target" "$fake/kio-rs/target"
STUB_LOG=$scratch/cargo.log
export STUB_LOG
cat >"$fake/ci/schedule.sh" <<'EOF'
#!/bin/sh
set -eu
[ "$1" = --prepare ]
[ -z "${LLVM_PROFILE_FILE:-}${CARGO_LLVM_COV:-}" ]
printf '%s\n' "$0"
EOF
cat >"$fake/ci/cargo.sh" <<'EOF'
#!/bin/sh
set -eu
case "$*" in
  'llvm-cov --version') exit 0 ;;
  *clean*) printf 'shared clean attempted\n' >>"$STUB_LOG"; exit 90 ;;
  'llvm-cov --release show-env --export-prefix'|'llvm-cov show-env --export-prefix')
    [ -n "${CARGO_TARGET_DIR:-}" ] || { printf 'show-env used ordinary target\n' >&2; exit 91; }
    target=$CARGO_TARGET_DIR
    [ "${SCENARIO:-}" != wrong-target ] || target=$PWD/target
    printf "export RUSTFLAGS='-C instrument-coverage'\nexport LLVM_PROFILE_FILE='%s/build.profraw'\nexport CARGO_LLVM_COV=1\nexport CARGO_LLVM_COV_TARGET_DIR='%s'\n" "$target" "$target"
    ;;
  'llvm-cov --release report --lcov --failure-mode=any')
    total=10
    hit=5
    for raw in "$CARGO_TARGET_DIR/with-kio-gen-"*.profraw; do
      [ -e "$raw" ] || continue
      hit=5
      for generated in "$CARGO_TARGET_DIR/with-kio-gen-gen-"*.profraw; do
        [ ! -e "$generated" ] || hit=6
      done
      [ "${SCENARIO:-}" != denominator ] || total=11
    done
    printf 'SF:src/normalization.rs\nDA:1,%s\nLF:%s\nLH:%s\nend_of_record\n' "$hit" "$total" "$hit"
    ;;
  build*)
    target=${CARGO_TARGET_DIR:-$PWD/target}
    case "$PWD:$*" in
      */kio-rs:'build --release --bin kio')
        [ "${RUSTFLAGS:-}" = '-C instrument-coverage' ] || exit 92
        case "$target" in */coverage) ;; *) exit 93 ;; esac
        mkdir -p "$target/release"
        printf '#!/bin/sh\n[ "${SCENARIO:-}" != no-profiles ] || exit 0\ncase "${SCENARIO:-}:$LLVM_PROFILE_FILE" in no-gen-profiles:*with-kio-gen-gen*) exit 0 ;; esac\nprintf profile >"$LLVM_PROFILE_FILE"\n' >"$target/release/kio"
        chmod +x "$target/release/kio"
        ;;
      *)
        [ -z "${LLVM_PROFILE_FILE:-}${CARGO_LLVM_COV:-}${RUSTFLAGS:-}" ] || exit 94
        for arg in "$@"; do
          if [ "${next:-}" = 1 ]; then target=$arg; next=0; fi
          [ "$arg" != --target-dir ] || next=1
        done
        mkdir -p "$target/release"
        case "$PWD" in
          */kio-gen-rs)
            printf '#!/bin/sh\nmkdir -p "$2"\n' >"$target/release/kio-gen"
            chmod +x "$target/release/kio-gen"
            ;;
          */kio-rs) printf '#!/bin/sh\nexit 0\n' >"$target/release/kio-prime" ;;
          */kio-prime-check-rs) printf '#!/bin/sh\nexit 0\n' >"$target/release/kio-prime-check" ;;
          */kio-test-runner-rs)
            printf '#!/bin/sh\nexit 0\n' >"$target/release/kio-test-runner-js"
            printf '#!/bin/sh\nexit 0\n' >"$target/release/kio-test-runner-rust"
            ;;
          *) : ;;
        esac
        ;;
    esac
    ;;
  *) printf 'unexpected stub cargo: %s\n' "$*" >&2; exit 95 ;;
esac
EOF
cat >"$fake/ci/run-tests.sh" <<'EOF'
#!/bin/sh
set -eu
[ -z "${LLVM_PROFILE_FILE:-}${CARGO_LLVM_COV:-}${RUSTFLAGS:-}" ] || exit 98
for arg in "$@"; do
  case "$arg" in
    --cases-dir=*) cases=${arg#*=} ;;
    --impl-def=*) compiler=${arg#*,kio=}; compiler=${compiler%%,*} ;;
  esac
done
if [ "${SCENARIO:-}" = zero-count ]; then
  case "$cases" in */kio-gen-batch) printf 'no cases found\n'; exit 0 ;; esac
fi
case "$compiler" in */run-*/bin/*/kio) ;; *) printf 'ordinary compiler executed\n' >&2; exit 96 ;; esac
case "$(sed -n '$p' "$compiler")" in "exec '"*"/run-"*"/bin/kio' "*) ;; *) exit 97 ;; esac
cohort=$(basename "$(dirname "$compiler")")
profile=$(sed -n "s/^LLVM_PROFILE_FILE='\(.*\)'$/\1/p" "$compiler")
[ "${profile##*/}" = "$cohort-%m.profraw" ] || { printf 'unbounded or foreign cohort profile pattern\n' >&2; exit 99; }
sh "$compiler"
name=golden
case "$cases" in */kio-gen-batch) name=generated ;; esac
case "${SCENARIO:-}:$compiler:$name" in
  selection:*with-kio-gen*:golden) name=different ;;
esac
printf 'pass [kio@js] %s\n  kio@js: 1 passed, 0 failed\n' "$name"
printf 'pass [invariants@%s] %s\n' "$(basename "$compiler")" "$name"
EOF

for SCENARIO in good zero-count wrong-target no-profiles no-gen-profiles selection denominator; do
  export SCENARIO
  count=1
  [ "$SCENARIO" != zero-count ] || count=0
  status=0
  sh "$fake/reports/kio-gen-coverage-delta.sh" --impls=kio@js "--count=$count" --seed=42 --keep-work \
    >"$scratch/$SCENARIO.log" 2>&1 || status=$?
  if [ "$SCENARIO" = good ] || [ "$SCENARIO" = zero-count ]; then
    [ "$status" = 0 ] || { cat "$scratch/$SCENARIO.log" >&2; exit 1; }
    delta=10.00
    [ "$SCENARIO" != zero-count ] || delta=0.00
    grep -q "kio-gen delta: +$delta%" "$scratch/$SCENARIO.log"
    [ ! -e "$STUB_LOG" ]
    cmp "$scratch/sentinel" "$scratch/ordinary-target/sentinel"
  else
    [ "$status" -ne 0 ] || { printf 'accepted invalid %s evidence\n' "$SCENARIO" >&2; exit 1; }
    case "$SCENARIO" in
      wrong-target) message='coverage target differs from isolated target' ;;
      no-profiles) message='missing baseline-golden profiles' ;;
      no-gen-profiles) message='missing with-kio-gen-gen profiles' ;;
      selection) message='golden execution selections differ between passes' ;;
      denominator) message='coverage denominators differ between passes' ;;
    esac
    grep -q "$message" "$scratch/$SCENARIO.log" || { cat "$scratch/$SCENARIO.log" >&2; exit 1; }
  fi
done
if grep -q '%p' "$REPO_ROOT/reports/equiv-eval-reachability.sh" \
  || ! grep -Fq 'unit-%m.profraw' "$REPO_ROOT/reports/equiv-eval-reachability.sh" \
  || [ "$(grep -Fc "\$prefix-%m.profraw" "$REPO_ROOT/reports/equiv-eval-reachability.sh")" -ne 2 ]; then
  printf 'normalizer cohort profiles require bounded merge pools\n' >&2; exit 1
fi
printf 'kio-gen-coverage-isolation-selftest: isolated execution and evidence rejection pass\n'
