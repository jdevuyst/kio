#!/bin/sh
#
# Exercise the published preflight against a controlled process list so the
# enclosing CI process does not affect the result.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
SKILL="$REPO_ROOT/ai/skills/clear-caches/SKILL.md"

extract_shell_fence() {
  heading=$1
  file=$2
  awk -v heading="$heading" '
    $0 == heading { in_section = 1; next }
    in_section && /^## / { exit }
    in_section && $0 == "```sh" { in_fence = 1; next }
    in_fence && $0 == "```" { exit }
    in_fence { print }
  ' "$file"
}

preflight=$(extract_shell_fence '## Preflight' "$SKILL")
run_fence=$(extract_shell_fence '## Run' "$SKILL")

[ -n "$preflight" ] || {
  printf 'clear-caches-preflight-selftest: missing Preflight shell fence\n' >&2
  exit 1
}
[ -n "$run_fence" ] || {
  printf 'clear-caches-preflight-selftest: missing Run shell fence\n' >&2
  exit 1
}
case $preflight in
  *'rm -rf'*|*'shared_root='*|*'docker '*)
    printf 'clear-caches-preflight-selftest: unsafe command crossed into Preflight extraction\n' >&2
    exit 1
    ;;
esac

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
mkdir -p "$scratch/bin" "$scratch/empty-bin" "$scratch/ps-only-bin" "$scratch/sentinel-bin"

ORIGINAL_PATH=$PATH
SH_BIN=/bin/sh
GREP_BIN=$(command -v grep)

cat >"$scratch/bin/ps" <<'EOF'
#!/bin/sh
set -eu

if [ "$#" -eq 7 ] &&
    [ "$1" = "-Aww" ] && [ "$2" = "-o" ] && [ "$3" = "pid=" ] &&
    [ "$4" = "-o" ] && [ "$5" = "ucomm=" ] &&
    [ "$6" = "-o" ] && [ "$7" = "args=" ]; then
  [ -z "${CLEAR_CACHES_SELFTEST_PS_SNAPSHOT_STATUS-}" ] ||
    exit "$CLEAR_CACHES_SELFTEST_PS_SNAPSHOT_STATUS"
  [ -n "${CLEAR_CACHES_SELFTEST_PROCESS_TABLE-}" ] || exit 0
  printf '%s\n' "${CLEAR_CACHES_SELFTEST_PROCESS_TABLE-}" |
    awk -F '|' '{
      process_pid = $1
      process_name = $2
      sub(/^[^|]*[|][^|]*[|]/, "")
      printf " %8s   %-19s   %s\n", process_pid, process_name, $0
    }'
else
  printf 'clear-caches-preflight-selftest: unexpected fake ps invocation\n' >&2
  exit 97
fi
EOF

cat >"$scratch/bin/grep" <<'EOF'
#!/bin/sh
set -eu

[ -z "${CLEAR_CACHES_SELFTEST_GREP_STATUS-}" ] ||
  exit "$CLEAR_CACHES_SELFTEST_GREP_STATUS"
PATH=${CLEAR_CACHES_SELFTEST_REAL_PATH:?}
export PATH
exec grep "$@"
EOF

cat >"$scratch/bin/rm" <<'EOF'
#!/bin/sh
printf 'clear-caches-preflight-selftest: refusing unexpected rm invocation\n' >&2
exit 99
EOF
chmod +x "$scratch/bin/ps" "$scratch/bin/grep" "$scratch/bin/rm"
ln -s "$scratch/bin/ps" "$scratch/ps-only-bin/ps"

run_preflight() {
  CLEAR_CACHES_SELFTEST_PROCESS_TABLE=$1 \
    CLEAR_CACHES_SELFTEST_PS_SNAPSHOT_STATUS=$2 \
    CLEAR_CACHES_SELFTEST_GREP_STATUS=$3 \
    CLEAR_CACHES_SELFTEST_REAL_PATH=$ORIGINAL_PATH \
    PATH="$scratch/bin:$ORIGINAL_PATH" \
    "$SH_BIN" -c "$preflight"
}

preflight_line=$(printf '%s' "$preflight" | tr '\n' ' ')
run_line=$(printf '%s' "$run_fence" | tr '\n' ' ')
inspector_process="sh -c $preflight_line"
inspector_record="100|sh|$inspector_process"

failures=0

record_failure() {
  printf 'clear-caches-preflight-selftest: FAIL — %s\n' "$1" >&2
  failures=$((failures + 1))
}

expect_pass() {
  label=$1
  process_records=$2
  process_table=$inspector_record
  [ -z "$process_records" ] || process_table="$process_table
$process_records"
  if ! run_preflight "$process_table" '' '' \
      >"$scratch/$label.stdout" 2>"$scratch/$label.stderr"; then
    record_failure "$label was conservatively refused"
  fi
}

expect_refusal() {
  label=$1
  process_record=$2
  snapshot_status=$3
  grep_status=$4
  diagnostic=$5
  if [ "$process_record" = "__EMPTY_SNAPSHOT__" ]; then
    process_table=
  elif [ -z "$process_record" ]; then
    process_table=$inspector_record
  else
    process_table="$process_record
$inspector_record"
  fi
  if run_preflight "$process_table" "$snapshot_status" "$grep_status" \
      >"$scratch/$label.stdout" 2>"$scratch/$label.stderr"; then
    record_failure "$label was not refused"
  else
    status=$?
    if [ "$status" -ne 1 ]; then
      record_failure "$label exited $status instead of 1"
    elif ! "$GREP_BIN" -q "$diagnostic" "$scratch/$label.stderr"; then
      record_failure "$label lacked its fail-closed diagnostic"
    fi
  fi
}

expect_pass inspector ''

active_diagnostic='a build or test process is active'
expect_refusal ci-direct '101|all.sh|/checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-shell '101|sh|/bin/sh /checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-shell-flags '101|dash|/bin/dash -eu /checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-bash-noprofile '101|bash|bash --noprofile ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-bash-euo '101|bash|bash -euo pipefail ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-zsh-long '101|zsh|zsh --no-rcs ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-zsh-attached '101|zsh|zsh -xoshwordsplit ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-mksh '101|mksh|/opt/bin/mksh ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-ksh93 '101|ksh93|/bin/ksh93 ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-versioned-shell '101|bash-5.2|/opt/shells/bash-5.2 ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-fish '101|fish|fish ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-cwd '101|sh|sh all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-cwd-dot '101|sh|sh ./all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-busybox-sh '101|busybox|/bin/busybox sh /checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-busybox-ash '101|busybox|/bin/busybox ash /checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-toybox-sh '101|toybox|/bin/toybox sh /checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal ci-shell-command-text '101|sh|sh -c printf /checkout/ci/all.sh' '' '' "$active_diagnostic"
expect_refusal ci-shell-option-value '101|bash|bash -o ci/all.sh' '' '' "$active_diagnostic"
expect_refusal cargo '101|cargo|/opt/rust/bin/cargo build' '' '' "$active_diagnostic"
expect_refusal scheduler-truncated '101|kio-ci-schedule|/cache/kio-ci-scheduler/bin/key/kio-ci-scheduler run --resource work -- test' '' '' "$active_diagnostic"
expect_refusal scheduler-full '101|kio-ci-scheduler|decorated-scheduler run --resource compiler -- test' '' '' "$active_diagnostic"
expect_refusal runner-truncated '101|kio-test-runner|decorated-runner --case=example' '' '' "$active_diagnostic"
expect_refusal macos-runner-full-ucomm '101|kio-test-runner-rust|decorated-runner --case=example' '' '' "$active_diagnostic"
expect_refusal macos-cargo-ucomm '101|cargo|decorated-worker --build' '' '' "$active_diagnostic"
expect_refusal ci-stable-ucomm '101|bash|decorated-shell /checkout/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"
expect_refusal cargo-space '101|cargo|/opt/Rust Tools/bin/cargo build' '' '' "$active_diagnostic"
expect_refusal runner-space '101|kio-test-runner|/checkout/Runner Tools/bin/decorated-runner --case=example' '' '' "$active_diagnostic"
expect_refusal shell-space '101|bash|/opt/Shell Tools/bin/bash /checkout/Work Tree/ci/all.sh SAMPLE_IMPL' '' '' "$active_diagnostic"

expect_pass combined "101|sh|sh -c $preflight_line $run_line"
expect_pass vim '101|vim|vim /checkout/ci/all.sh'
expect_pass rg-ci '101|rg|rg ci/all.sh /checkout'
expect_pass rg-cargo '101|rg|rg cargo /checkout'
expect_pass rg-sibling '101|rg|rg kio-test-runner- /sibling/worktree'
expect_pass scheduler-prefix-collision '101|kio-ci-schedule|unrelated-process --flag'
expect_pass cache-path '101|du|du -sh /cache/kio-test-runner-rust'
expect_pass cache-ci '101|du|du -sh /cache/ci/all.sh'
expect_pass python '101|python|python /checkout/ci/all.sh'
expect_pass shell-nonnumeric-suffix '101|bashful|/opt/bin/bashful /checkout/ci/all.sh'
expect_pass busybox-non-shell '101|busybox|busybox echo /checkout/ci/all.sh'
expect_pass toybox-non-shell '101|toybox|toybox grep ci/all.sh'
expect_pass ssh '101|ssh|ssh host ci/all.sh'
expect_pass ssh-versioned '101|ssh-9.8|ssh-9.8 host ci/all.sh'
expect_pass rsh '101|rsh|rsh host ci/all.sh'
expect_pass rsh-versioned '101|rsh2|rsh2 host ci/all.sh'
expect_pass autossh '101|autossh|autossh host ci/all.sh'
expect_pass autossh-versioned '101|autossh-1.4|autossh-1.4 host ci/all.sh'

inspection_diagnostic='process inspection failed'
expect_refusal malformed-pid 'not-a-pid|bash|bash ci/all.sh' '' '' "$inspection_diagnostic"
expect_refusal malformed-missing-args '101|bash|' '' '' "$inspection_diagnostic"
expect_refusal ps-snapshot-error '' 2 '' "$inspection_diagnostic"
expect_refusal empty-snapshot '__EMPTY_SNAPSHOT__' '' '' "$inspection_diagnostic"
expect_refusal grep-error '101|bash|bash ci/all.sh' '' 2 "$inspection_diagnostic"

if CLEAR_CACHES_SELFTEST_PROCESS_TABLE=$inspector_record \
    PATH="$scratch/empty-bin" \
    "$SH_BIN" -c "$preflight" \
    >"$scratch/ps-missing.stdout" 2>"$scratch/ps-missing.stderr"; then
  record_failure 'missing ps failed open'
else
  status=$?
  if [ "$status" -ne 1 ]; then
    record_failure "missing ps exited $status instead of 1"
  elif ! "$GREP_BIN" -q 'ps is unavailable' "$scratch/ps-missing.stderr"; then
    record_failure 'missing ps lacked its fail-closed diagnostic'
  fi
fi

if CLEAR_CACHES_SELFTEST_PROCESS_TABLE=$inspector_record \
    PATH="$scratch/ps-only-bin" \
    "$SH_BIN" -c "$preflight" \
    >"$scratch/grep-missing.stdout" 2>"$scratch/grep-missing.stderr"; then
  record_failure 'missing grep failed open'
else
  status=$?
  if [ "$status" -ne 1 ]; then
    record_failure "missing grep exited $status instead of 1"
  elif ! "$GREP_BIN" -q 'grep is unavailable' "$scratch/grep-missing.stderr"; then
    record_failure 'missing grep lacked its fail-closed diagnostic'
  fi
fi

cat >"$scratch/no-preflight-fence.md" <<'EOF'
## Preflight

No executable fence.

## Run

```sh
rm -rf should-not-run
```
EOF

# Keep an extractor regression observable without letting the Run fixture delete.
cat >"$scratch/sentinel-bin/rm" <<'EOF'
#!/bin/sh
: >"${CLEAR_CACHES_SELFTEST_SENTINEL:?}"
EOF
chmod +x "$scratch/sentinel-bin/rm"

candidate=$(extract_shell_fence '## Preflight' "$scratch/no-preflight-fence.md")
sentinel="$scratch/run-fence-reached"
CLEAR_CACHES_SELFTEST_SENTINEL=$sentinel \
  PATH="$scratch/sentinel-bin:$ORIGINAL_PATH" \
  "$SH_BIN" -c "$candidate"

if [ -n "$candidate" ]; then
  record_failure 'Preflight extraction crossed into the Run section'
fi
if [ -e "$sentinel" ]; then
  record_failure 'missing Preflight fence reached the Run sentinel'
fi

if [ "$failures" -ne 0 ]; then
  printf 'clear-caches-preflight-selftest: %s failure(s)\n' "$failures" >&2
  exit 1
fi

printf 'clear-caches-preflight-selftest: ok\n'
