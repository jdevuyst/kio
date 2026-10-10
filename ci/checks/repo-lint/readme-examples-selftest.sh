#!/bin/sh
# shellcheck disable=SC2016 # Markdown fences are literal backticks.
# Verify README extraction and compiler failure propagation without a build.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
CHECK="$REPO_ROOT/ci/infra/check-readme-examples.sh"
scratch=$(mktemp -d)
cleanup_readme_examples_selftest() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_readme_examples_selftest EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

cat >"$scratch/kio" <<'SH'
#!/bin/sh
set -eu
printf '%s\n' "$PWD:$*" >>"$README_TEST_CALLS"
case "$*" in
  check)
    if [ "${README_TEST_EXACT:-0}" = 1 ]; then
      for source in ./*.kio; do
        cmp "$source" "$README_TEST_EXPECTED/$(basename -- "$source")"
      done
    fi
    if grep -F INVALID ./*.kio >/dev/null; then
      printf 'invalid example\n' >&2
      exit 14
    fi
    ;;
  'fmt --check '* )
    if grep -F NONCANONICAL "$3" >/dev/null; then
      printf '%s\n' "$3"
      exit 60
    fi
    if [ "${README_TEST_FMT_FAILURE:-0}" = 1 ]; then
      printf 'formatter failed\n' >&2
      exit 1
    fi
    ;;
  *) exit 2 ;;
esac
SH
chmod +x "$scratch/kio"
README_TEST_CALLS=$scratch/calls
export README_TEST_CALLS
README_TEST_EXPECTED=$scratch/expected
README_TEST_EXACT=1
export README_TEST_EXPECTED README_TEST_EXACT
mkdir -p "$README_TEST_EXPECTED"
printf 'module first;\nfn unit() { () }\n' >"$README_TEST_EXPECTED/first.kio"
printf 'module second;\nfn unit() { () }\n' >"$README_TEST_EXPECTED/second.kio"

cat >"$scratch/README.md" <<'MD'
```text
~~~kio
not an example
~~~
```

```kio
module first;
fn unit() { () }
```

~~~~kio
module second;
fn unit() { () }
~~~~
MD
sh "$CHECK" "$scratch/README.md" "$scratch/kio" >"$scratch/output"
test "$(wc -l <"$README_TEST_CALLS" | tr -d ' ')" = 4
grep -F 'example-1:check' "$README_TEST_CALLS" >/dev/null
grep -F 'example-2:fmt --check second.kio' "$README_TEST_CALLS" >/dev/null
README_TEST_EXACT=0

expect_failure() {
  expected=$1
  diagnostic=$2
  status=0
  sh "$CHECK" "$scratch/README.md" "$scratch/kio" >"$scratch/output" 2>&1 || status=$?
  if [ "$status" -ne "$expected" ]; then
    printf 'readme-examples-selftest: returned %s, expected %s\n' "$status" "$expected" >&2
    exit 1
  fi
  grep -F "$diagnostic" "$scratch/output" >/dev/null
}

printf '# No examples\n' >"$scratch/README.md"
expect_failure 1 'no Kio examples found'
printf '```kio\nmodule unfinished;\n' >"$scratch/README.md"
expect_failure 1 'unclosed Kio example'
printf '```kio\nfn partial() { () }\n```\n' >"$scratch/README.md"
expect_failure 1 'needs a complete module declaration'
printf '```kio\nmodule invalid;\nINVALID\n```\n' >"$scratch/README.md"
expect_failure 14 'invalid example'
printf '```kio\nmodule dirty;\nNONCANONICAL\n```\n' >"$scratch/README.md"
expect_failure 60 'dirty.kio'
printf '```kio\nmodule valid;\n```\n' >"$scratch/README.md"
README_TEST_FMT_FAILURE=1
export README_TEST_FMT_FAILURE
expect_failure 1 'formatter failed'
printf 'readme-examples-selftest: OK\n'
