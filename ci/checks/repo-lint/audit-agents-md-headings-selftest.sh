#!/bin/sh
#
# Exercise the shared AGENTS.md outbound-heading validator against both
# citation classes and every available portable awk implementation.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
VALIDATOR="$REPO_ROOT/ci/checks/repo-lint/audit-agents-md-headings.awk"

find_on_path() (
  name=$1
  IFS=:
  for directory in $PATH; do
    [ -n "$directory" ] || directory=.
    if [ ! -f "$directory/$name" ] || [ ! -x "$directory/$name" ]; then
      continue
    fi
    absolute_directory=$(CDPATH='' cd -P "$directory" 2>/dev/null && pwd) ||
      continue
    printf '%s/%s\n' "$absolute_directory" "$name"
    exit 0
  done
  exit 1
)

SYSTEM_AWK=$(find_on_path awk) || {
  printf 'audit-agents-md-headings-selftest: awk not found on PATH\n' >&2
  exit 1
}
SYSTEM_SH=$(find_on_path sh) || {
  printf 'audit-agents-md-headings-selftest: sh not found on PATH\n' >&2
  exit 1
}
if [ "${KIO_AUDIT_HEADINGS_BUSYBOX_OUTER_CHILD-}" = 1 ]; then
  BUSYBOX=$(find_on_path busybox)
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
mkdir -p "$scratch/awk-bin"

backtick=$(printf '\140')
dollar=$(printf '\044')
double_backticks="${backtick}${backtick}"
triple_backticks="${double_backticks}${backtick}"
quadruple_backticks="${triple_backticks}${backtick}"
quintuple_backticks="${quadruple_backticks}${backtick}"

printf '%s\n' \
  '#!/bin/sh' \
  'set -eu' \
  "impl=${dollar}{KIO_AUDIT_AWK_IMPL:?}" \
  "case \"${dollar}impl\" in" \
  '  /*) ;;' \
  '  *) exit 98 ;;' \
  'esac' \
  "case ${dollar}{KIO_AUDIT_AWK_MODE:?} in" \
  "  direct) exec \"${dollar}impl\" \"${dollar}@\" ;;" \
  "  busybox) exec \"${dollar}impl\" awk \"${dollar}@\" ;;" \
  '  *) exit 97 ;;' \
  'esac' \
  >"$scratch/awk-bin/awk"
chmod +x "$scratch/awk-bin/awk"
validator_command="exec awk -f \"${dollar}1\" AGENTS.md"

failures=0

record_failure() {
  printf 'audit-agents-md-headings-selftest: FAIL — %s\n' "$1" >&2
  failures=$((failures + 1))
}

make_source_fixture() {
  label=$1
  source_body=$2
  target_file=$3
  target_body=$4
  fixture="$scratch/$label"
  mkdir -p "$fixture/${target_file%/*}"
  printf '# Agent instructions\n\n%s\n' "$source_body" >"$fixture/AGENTS.md"
  printf '# Fixture target\n\n%s\n' "$target_body" >"$fixture/$target_file"
}

make_fixture() {
  label=$1
  citation=$2
  target_file=$3
  target_body=$4
  make_source_fixture "$label" "## Universal rules

- $citation" "$target_file" "$target_body"
}

make_trigger_fixture() {
  label=$1
  citation=$2
  target_file=$3
  target_body=$4
  make_source_fixture "$label" "## Trigger table

| Action | Topic file |
| --- | --- |
| Check headings | $citation |" "$target_file" "$target_body"
}

make_prose_fixture() {
  label=$1
  citation=$2
  target_file=$3
  target_body=$4
  make_source_fixture "$label" "## About this file

$citation" "$target_file" "$target_body"
}

run_validator() {
  label=$1
  shell_impl=${2:-$SYSTEM_SH}
  awk_impl=${3:-$SYSTEM_AWK}
  awk_mode=${4:-direct}
  locale_name=${5:-C}
  fixture="$scratch/$label"
  (
    cd "$fixture"
    # Bound a possibly recursive awk-wrapper dispatch, not the total time a
    # finite suite takes while sharing a busy runner.
    set -- "$shell_impl" -c "$validator_command" sh "$VALIDATOR"
    if [ "${KIO_AUDIT_HEADINGS_BUSYBOX_OUTER_CHILD-}" = 1 ]; then
      set -- "$BUSYBOX" timeout -k 1 30 "$@"
    fi
    LC_ALL=$locale_name \
      KIO_AUDIT_AWK_IMPL=$awk_impl \
      KIO_AUDIT_AWK_MODE=$awk_mode \
      PATH="$scratch/awk-bin:$PATH" \
      "$@"
  ) >"$fixture/stdout" 2>"$fixture/stderr"
}

expect_clean() {
  label=$1
  shell_impl=${2:-$SYSTEM_SH}
  awk_impl=${3:-$SYSTEM_AWK}
  awk_mode=${4:-direct}
  locale_name=${5:-C}
  implementation=${6:-system-sh/system-awk/C}
  if ! run_validator "$label" "$shell_impl" "$awk_impl" "$awk_mode" \
      "$locale_name"; then
    record_failure "$label ($implementation) exited nonzero"
  elif [ -s "$scratch/$label/stdout" ] || [ -s "$scratch/$label/stderr" ]; then
    record_failure "$label ($implementation) emitted a false finding or tool error"
  fi
}

expect_missing() {
  label=$1
  expected=$2
  shell_impl=${3:-$SYSTEM_SH}
  awk_impl=${4:-$SYSTEM_AWK}
  awk_mode=${5:-direct}
  locale_name=${6:-C}
  implementation=${7:-system-sh/system-awk/C}
  if ! run_validator "$label" "$shell_impl" "$awk_impl" "$awk_mode" \
      "$locale_name"; then
    record_failure "$label ($implementation) exited nonzero"
  elif [ -s "$scratch/$label/stderr" ]; then
    record_failure "$label ($implementation) emitted an unexpected tool error"
  else
    actual=$(sed -n '1,$p' "$scratch/$label/stdout")
    [ "$actual" = "$expected" ] ||
      record_failure "$label ($implementation) did not report exactly the missing heading"
  fi
}

for kind in spec topic; do
  case $kind in
    spec)
      target_file=specs/grammar.md
      source_link='[syntax](specs/grammar.md) §'
      missing_prefix='AGENTS.md cites spec heading not found: specs/grammar.md §'
      ;;
    topic)
      target_file=ai/topics/local-ci.md
      source_link='[local CI](ai/topics/local-ci.md) §'
      missing_prefix='AGENTS.md cites topic heading not found: ai/topics/local-ci.md §'
      ;;
  esac

  make_trigger_fixture "$kind-trigger-row" \
    "$source_link Trigger heading." "$target_file" '## Trigger heading'
  expect_clean "$kind-trigger-row"

  make_trigger_fixture "$kind-trigger-row-stale" \
    "$source_link Missing heading." "$target_file" '## Present heading'
  expect_missing "$kind-trigger-row-stale" "$missing_prefix Missing heading"

  make_prose_fixture "$kind-non-rule-prose-is-out-of-scope" \
    "$source_link Missing heading." "$target_file" '## Present heading'
  expect_clean "$kind-non-rule-prose-is-out-of-scope"

  make_source_fixture "$kind-source-rule-fence-markers" \
    "## Universal rules

${triple_backticks}markdown
- ${source_link} Missing heading.
${triple_backticks}" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-rule-fence-markers"

  make_source_fixture "$kind-source-trigger-fence-markers" \
    "## Trigger table

${triple_backticks}markdown
| Example | ${source_link} Missing heading. |
${triple_backticks}" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-trigger-fence-markers"

  make_fixture "$kind-prose-tail" \
    "$source_link Durable broad runs. The next sentence is prose." \
    "$target_file" '## Durable broad runs'
  expect_clean "$kind-prose-tail"

  make_fixture "$kind-literal-metacharacters" \
    "$source_link Values [A-Z].* (x) + y?." \
    "$target_file" '## Values [A-Z].* (x) + y?'
  expect_clean "$kind-literal-metacharacters"

  make_fixture "$kind-documented-em-dash-prefix" \
    "$source_link The escape hatch." \
    "$target_file" '## The escape hatch — completeness through the last resort'
  expect_clean "$kind-documented-em-dash-prefix"

  make_fixture "$kind-stale-em-dash-description" \
    "$source_link Missing heading — explanatory tail." \
    "$target_file" '## Present heading'
  expect_missing "$kind-stale-em-dash-description" \
    "$missing_prefix Missing heading"

  make_fixture "$kind-lexical-prefix-collision" \
    "$source_link Cache." "$target_file" '## Cacheable values'
  expect_missing "$kind-lexical-prefix-collision" "$missing_prefix Cache"

  make_fixture "$kind-embedded-substring-collision" \
    "$source_link Script portability." \
    "$target_file" '## Shared script portability'
  expect_missing "$kind-embedded-substring-collision" \
    "$missing_prefix Script portability"

  make_fixture "$kind-punctuation-without-boundary" \
    "$source_link Cache.value." "$target_file" '## Cache'
  expect_missing "$kind-punctuation-without-boundary" \
    "$missing_prefix Cache.value"

  make_fixture "$kind-internal-punctuation" \
    "$source_link Why? name=value [A-Z].*." \
    "$target_file" '## Why? name=value [A-Z].*'
  expect_clean "$kind-internal-punctuation"

  make_fixture "$kind-inline-code-boundary-collision" \
    "$source_link ${backtick}foo: bar${backtick}." \
    "$target_file" '## foo'
  expect_missing "$kind-inline-code-boundary-collision" \
    "$missing_prefix ${backtick}foo: bar${backtick}"

  make_fixture "$kind-inline-code-exact-heading" \
    "$source_link ${backtick}foo: bar${backtick}." \
    "$target_file" '## foo: bar'
  expect_clean "$kind-inline-code-exact-heading"

  make_fixture "$kind-backslash-before-code-close" \
    "$source_link ${backtick}foo\\${backtick}." \
    "$target_file" "## foo\\"
  expect_clean "$kind-backslash-before-code-close"

  make_fixture "$kind-longer-code-span-content-backticks" \
    "$source_link ${double_backticks}shape ${backtick}x${backtick}${double_backticks}." \
    "$target_file" '## shape x'
  expect_missing "$kind-longer-code-span-content-backticks" \
    "$missing_prefix ${double_backticks}shape ${backtick}x${backtick}${double_backticks}"

  make_fixture "$kind-exact-longer-code-span" \
    "$source_link ${double_backticks}shape ${backtick}x${backtick}${double_backticks}." \
    "$target_file" \
    "## ${double_backticks}shape ${backtick}x${backtick}${double_backticks}"
  expect_clean "$kind-exact-longer-code-span"

  make_fixture "$kind-heading-dash-inside-code" \
    "$source_link foo." "$target_file" \
    "## ${backtick}foo — hidden${backtick}"
  expect_missing "$kind-heading-dash-inside-code" "$missing_prefix foo"

  make_fixture "$kind-heading-dash-after-code" \
    "$source_link foo." "$target_file" \
    "## ${backtick}foo${backtick} — details"
  expect_clean "$kind-heading-dash-after-code"

  make_fixture "$kind-six-hash-heading" \
    "$source_link Deep heading." "$target_file" \
    '###### Deep heading'
  expect_clean "$kind-six-hash-heading"

  make_fixture "$kind-seven-hash-pseudo-heading" \
    "$source_link Not a heading." "$target_file" \
    '####### Not a heading'
  expect_missing "$kind-seven-hash-pseudo-heading" \
    "$missing_prefix Not a heading"

  make_fixture "$kind-parenthesized-heading" \
    "$source_link Shell support (Git Bash)." \
    "$target_file" '## Shell support (Git Bash)'
  expect_clean "$kind-parenthesized-heading"

  make_fixture "$kind-pipe-heading" \
    "$source_link A | B." "$target_file" '## A | B'
  expect_clean "$kind-pipe-heading"

  make_fixture "$kind-pipe-suffix-mismatch" \
    "$source_link A | C." "$target_file" '## A | B'
  expect_missing "$kind-pipe-suffix-mismatch" "$missing_prefix A | C"

  make_fixture "$kind-fenced-backtick-heading" \
    "$source_link Fenced backtick heading." "$target_file" \
    "   ${triple_backticks}kio
## Fenced backtick heading
${triple_backticks}"
  expect_missing "$kind-fenced-backtick-heading" \
    "$missing_prefix Fenced backtick heading"

  make_fixture "$kind-shorter-backtick-close" \
    "$source_link Still in backtick fence." "$target_file" \
    "${quadruple_backticks} kio
## First fenced heading
${triple_backticks}
## Still in backtick fence
${quintuple_backticks}"
  expect_missing "$kind-shorter-backtick-close" \
    "$missing_prefix Still in backtick fence"

  make_fixture "$kind-shorter-tilde-close" \
    "$source_link Still in tilde fence." "$target_file" \
    '~~~~ markdown
## First fenced heading
~~~
## Still in tilde fence
~~~~~'
  expect_missing "$kind-shorter-tilde-close" \
    "$missing_prefix Still in tilde fence"

  make_fixture "$kind-real-heading-after-fences" \
    "$source_link Real heading." "$target_file" \
    "${quadruple_backticks} text
## Fenced backtick heading
${quintuple_backticks}
~~~~ markdown
## Fenced tilde heading
~~~~~
## Real heading"
  expect_clean "$kind-real-heading-after-fences"

  make_fixture "$kind-source-inline-pseudo-citation" \
    "${backtick}${source_link} Missing heading.${backtick}" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-inline-pseudo-citation"

  make_fixture "$kind-source-multiline-code-pseudo-citation" \
    "${backtick}example
${source_link} Missing heading.
${backtick}" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-multiline-code-pseudo-citation"

  make_fixture "$kind-source-backslash-before-code-close" \
    "${backtick}example\\${backtick} ${source_link} Missing heading." \
    "$target_file" '## Present heading'
  expect_missing "$kind-source-backslash-before-code-close" \
    "$missing_prefix Missing heading"

  make_fixture "$kind-source-unmatched-backtick" \
    "unmatched ${backtick}; ${source_link} Missing heading." \
    "$target_file" '## Present heading'
  expect_missing "$kind-source-unmatched-backtick" \
    "$missing_prefix Missing heading"

  make_fixture "$kind-source-escaped-opening-backtick" \
    "\\${backtick}${source_link} Missing heading. ${backtick}" \
    "$target_file" '## Present heading'
  expect_missing "$kind-source-escaped-opening-backtick" \
    "$missing_prefix Missing heading"

  make_fixture "$kind-source-code-span-does-not-cross-list-items" \
    "unmatched ${backtick}
- ${source_link} Missing heading. ${backtick}" \
    "$target_file" '## Present heading'
  expect_missing "$kind-source-code-span-does-not-cross-list-items" \
    "$missing_prefix Missing heading"

  make_fixture "$kind-source-code-span-after-unmatched-paragraph" \
    "unmatched ${backtick}

${backtick}${source_link} Missing heading.${backtick}" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-code-span-after-unmatched-paragraph"

  make_fixture "$kind-source-backtick-fenced-pseudo-citation" \
    "${triple_backticks}markdown
  ${source_link} Missing heading.
  ${triple_backticks}" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-backtick-fenced-pseudo-citation"

  make_fixture "$kind-source-tilde-fenced-pseudo-citation" \
    "~~~~ markdown
  ${source_link} Missing heading.
  ~~~~" \
    "$target_file" '## Present heading'
  expect_clean "$kind-source-tilde-fenced-pseudo-citation"

  make_fixture "$kind-windows-backslash" \
    "$source_link Windows C:\path." "$target_file" '## Windows C:\path'
  expect_clean "$kind-windows-backslash"

  make_fixture "$kind-windows-backslash-mismatch" \
    "$source_link Windows C:\path." "$target_file" '## Windows C:path'
  expect_missing "$kind-windows-backslash-mismatch" \
    "$missing_prefix Windows C:\path"

  make_fixture "$kind-shell-injection" \
    "$source_link Shell ${dollar}(touch PWNED-SHELL)." \
    "$target_file" "## Shell ${dollar}(touch PWNED-SHELL)"
  expect_clean "$kind-shell-injection"
  [ ! -e "$scratch/$kind-shell-injection/PWNED-SHELL" ] ||
    record_failure "$kind-shell-injection executed citation text"

  make_fixture "$kind-awk-injection" \
    "$source_link Quote \"); system(\"touch PWNED-AWK\"); #." \
    "$target_file" '## Quote "); system("touch PWNED-AWK"); #'
  expect_clean "$kind-awk-injection"
  [ ! -e "$scratch/$kind-awk-injection/PWNED-AWK" ] ||
    record_failure "$kind-awk-injection executed citation text"
done

make_fixture spec-labelled-real-spelling \
  "[${backtick}specs/language.md${backtick} § Open-world design](specs/language.md#open-world-design)" \
  specs/language.md '## Open-world design'
expect_clean spec-labelled-real-spelling

make_fixture spec-labelled-uppercase-subdirectory \
  "[${backtick}specs/Backends/HTTP-API.md${backtick} § Header](specs/Backends/HTTP-API.md#header)" \
  specs/Backends/HTTP-API.md '## Header'
expect_clean spec-labelled-uppercase-subdirectory

make_fixture spec-plain-uppercase-subdirectory \
  '[HTTP API](specs/Backends/HTTP-API.md) § Header.' \
  specs/Backends/HTTP-API.md '## Header'
expect_clean spec-plain-uppercase-subdirectory

make_fixture spec-labelled-stale-heading \
  "[${backtick}specs/Backends/HTTP-API.md${backtick} § Removed header](specs/Backends/HTTP-API.md#removed-header)" \
  specs/Backends/HTTP-API.md '## Present header'
expect_missing spec-labelled-stale-heading \
  'AGENTS.md cites spec heading not found: specs/Backends/HTTP-API.md § Removed header'

make_fixture spec-labelled-punctuation-prefix-collision \
  "[${backtick}specs/grammar.md${backtick} § Why? missing](specs/grammar.md#why-missing)" \
  specs/grammar.md '## Why'
expect_missing spec-labelled-punctuation-prefix-collision \
  'AGENTS.md cites spec heading not found: specs/grammar.md § Why? missing'

make_fixture spec-labelled-target-em-dash-suffix \
  "[${backtick}specs/grammar.md${backtick} § Why? missing](specs/grammar.md#why-missing)" \
  specs/grammar.md '## Why? missing — details'
expect_clean spec-labelled-target-em-dash-suffix

make_fixture spec-assignment-looking \
  '[syntax](specs/grammar.md) § name=value -v=citation -- assignment.' \
  specs/grammar.md '## name=value -v=citation -- assignment'
expect_clean spec-assignment-looking

make_fixture spec-labelled-inline-close-sequence \
  "[${backtick}specs/grammar.md${backtick} § shape ${backtick}](${backtick} tail](specs/grammar.md#shape-tail)" \
  specs/grammar.md "## shape ${backtick}](${backtick} tail"
expect_clean spec-labelled-inline-close-sequence

make_fixture spec-labelled-escaped-close-sequence \
  "[${backtick}specs/grammar.md${backtick} § shape \\]( tail](specs/grammar.md#shape-tail)" \
  specs/grammar.md '## shape \]( tail'
expect_clean spec-labelled-escaped-close-sequence

make_fixture spec-labelled-windows-backslash \
  "[${backtick}specs/grammar.md${backtick} § Windows C:\path](specs/grammar.md#windows-cpath)" \
  specs/grammar.md '## Windows C:\path'
expect_clean spec-labelled-windows-backslash

make_fixture spec-labelled-windows-backslash-mismatch \
  "[${backtick}specs/grammar.md${backtick} § Windows C:\path](specs/grammar.md#windows-cpath)" \
  specs/grammar.md '## Windows C:path'
expect_missing spec-labelled-windows-backslash-mismatch \
  'AGENTS.md cites spec heading not found: specs/grammar.md § Windows C:\path'

make_fixture spec-source-inline-labelled-pseudo-citation \
  "${double_backticks}[${backtick}specs/grammar.md${backtick} § Missing heading](specs/grammar.md#missing-heading)${double_backticks}" \
  specs/grammar.md '## Present heading'
expect_clean spec-source-inline-labelled-pseudo-citation

make_fixture spec-missing-file \
  '[missing](specs/Missing/FILE.md) § Missing heading.' \
  specs/fixture.md '## Unrelated heading'
expect_missing spec-missing-file \
  'AGENTS.md cites missing spec file: specs/Missing/FILE.md  (from: (specs/Missing/FILE.md) § Missing heading.)'

make_fixture spec-noncanonical-path \
  '[escape](specs/../grammar.md) § Grammar.' \
  specs/grammar.md '## Grammar'
expect_clean spec-noncanonical-path

make_fixture spec-multiple-citations \
  "[${backtick}specs/one.md${backtick} § First heading](specs/one.md#first-heading); [two](specs/two.md) § Missing heading." \
  specs/one.md '## First heading'
mkdir -p "$scratch/spec-multiple-citations/specs"
printf '# Two\n\n## Present heading\n' \
  >"$scratch/spec-multiple-citations/specs/two.md"
expect_missing spec-multiple-citations \
  'AGENTS.md cites spec heading not found: specs/two.md § Missing heading'

make_fixture topic-missing-file \
  '[missing](ai/topics/missing.md) § Missing heading.' \
  ai/topics/local-ci.md '## Unrelated heading'
expect_missing topic-missing-file \
  'AGENTS.md cites missing topic file: ai/topics/missing.md  (from: (ai/topics/missing.md) § Missing heading.)'

make_fixture topic-noncanonical-path \
  '[escape](ai/topics/../local-ci.md) § Durable broad runs.' \
  ai/topics/local-ci.md '## Durable broad runs'
expect_clean topic-noncanonical-path

make_fixture topic-empty-file \
  '[empty](ai/topics/empty.md) § Empty heading.' \
  ai/topics/local-ci.md '## Unrelated heading'
mkdir -p "$scratch/topic-empty-file/ai/topics"
: >"$scratch/topic-empty-file/ai/topics/empty.md"
expect_missing topic-empty-file \
  'AGENTS.md cites topic heading not found: ai/topics/empty.md § Empty heading'

make_fixture topic-multiple-citations \
  '[one](ai/topics/one.md) § First heading; [two](ai/topics/two.md) § Missing heading.' \
  ai/topics/local-ci.md '## Unrelated heading'
mkdir -p "$scratch/topic-multiple-citations/ai/topics"
printf '# One\n\n## First heading\n' \
  >"$scratch/topic-multiple-citations/ai/topics/one.md"
printf '# Two\n\n## Present heading\n' \
  >"$scratch/topic-multiple-citations/ai/topics/two.md"
expect_missing topic-multiple-citations \
  'AGENTS.md cites topic heading not found: ai/topics/two.md § Missing heading'

exercise_portable_fixtures() {
  implementation=$1
  shell_impl=$2
  awk_impl=$3
  awk_mode=$4
  locale_name=$5
  for kind in spec topic; do
    case $kind in
      spec)
        missing_prefix='AGENTS.md cites spec heading not found: specs/grammar.md §'
        ;;
      topic)
        missing_prefix='AGENTS.md cites topic heading not found: ai/topics/local-ci.md §'
        ;;
    esac
    expect_clean "$kind-trigger-row" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-trigger-row-stale" "$missing_prefix Missing heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-non-rule-prose-is-out-of-scope" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-rule-fence-markers" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-trigger-fence-markers" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-literal-metacharacters" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-backslash-before-code-close" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-longer-code-span-content-backticks" \
      "$missing_prefix ${double_backticks}shape ${backtick}x${backtick}${double_backticks}" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-exact-longer-code-span" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-heading-dash-inside-code" "$missing_prefix foo" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-heading-dash-after-code" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-six-hash-heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-seven-hash-pseudo-heading" \
      "$missing_prefix Not a heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-fenced-backtick-heading" \
      "$missing_prefix Fenced backtick heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-shorter-backtick-close" \
      "$missing_prefix Still in backtick fence" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-shorter-tilde-close" \
      "$missing_prefix Still in tilde fence" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-real-heading-after-fences" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-inline-pseudo-citation" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-multiline-code-pseudo-citation" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-source-backslash-before-code-close" \
      "$missing_prefix Missing heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-source-unmatched-backtick" \
      "$missing_prefix Missing heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-source-escaped-opening-backtick" \
      "$missing_prefix Missing heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-source-code-span-does-not-cross-list-items" \
      "$missing_prefix Missing heading" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-code-span-after-unmatched-paragraph" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-backtick-fenced-pseudo-citation" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-source-tilde-fenced-pseudo-citation" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_clean "$kind-windows-backslash" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
    expect_missing "$kind-windows-backslash-mismatch" \
      "$missing_prefix Windows C:\path" \
      "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
  done
  expect_clean spec-labelled-inline-close-sequence \
    "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
  expect_clean spec-labelled-escaped-close-sequence \
    "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
  expect_clean spec-source-inline-labelled-pseudo-citation \
    "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
  expect_missing spec-labelled-punctuation-prefix-collision \
    'AGENTS.md cites spec heading not found: specs/grammar.md § Why? missing' \
    "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
  expect_clean spec-labelled-target-em-dash-suffix \
    "$shell_impl" "$awk_impl" "$awk_mode" "$locale_name" "$implementation"
}

exercise_portable_matrix() {
  exercise_portable_fixtures system-sh/system-awk/C \
    "$SYSTEM_SH" "$SYSTEM_AWK" direct C

  for shell_name in dash bash; do
    if shell_impl=$(find_on_path "$shell_name" 2>/dev/null); then
      exercise_portable_fixtures "$shell_name/system-awk/C" \
        "$shell_impl" "$SYSTEM_AWK" direct C
    fi
  done

  for awk_name in gawk mawk; do
    if awk_impl=$(find_on_path "$awk_name" 2>/dev/null); then
      exercise_portable_fixtures "system-sh/$awk_name/C" \
        "$SYSTEM_SH" "$awk_impl" direct C
    fi
  done

  if busybox_impl=$(find_on_path busybox 2>/dev/null) &&
      "$busybox_impl" awk 'BEGIN { exit }' </dev/null; then
    exercise_portable_fixtures system-sh/busybox-awk/C \
      "$SYSTEM_SH" "$busybox_impl" busybox C
  fi

  if find_on_path locale >/dev/null 2>&1; then
    for locale_name in C.UTF-8 C.utf8; do
      if locale -a 2>/dev/null | grep -Fqx "$locale_name"; then
        exercise_portable_fixtures "system-sh/system-awk/$locale_name" \
          "$SYSTEM_SH" "$SYSTEM_AWK" direct "$locale_name"
        break
      fi
    done
  fi
}

# The outer-shell child exercises every canonical case above. The validator's
# shell/awk/locale matrix is independent of that outer shell and runs once.
if [ "${KIO_AUDIT_HEADINGS_BUSYBOX_OUTER_CHILD-}" != 1 ]; then
  exercise_portable_matrix
fi

if [ "${KIO_AUDIT_HEADINGS_BUSYBOX_OUTER_CHILD-}" != 1 ] &&
    busybox_impl=$(find_on_path busybox 2>/dev/null) &&
    "$busybox_impl" awk 'BEGIN { exit }' </dev/null &&
    "$busybox_impl" timeout -k 1 1 "$busybox_impl" true; then
  busybox_stdout="$scratch/busybox-outer.stdout"
  busybox_stderr="$scratch/busybox-outer.stderr"
  if ! KIO_AUDIT_HEADINGS_BUSYBOX_OUTER_CHILD=1 \
      "$busybox_impl" sh "$0" \
      >"$busybox_stdout" 2>"$busybox_stderr"; then
    record_failure 'outer BusyBox sh failed its canonical fixtures'
  elif [ -s "$busybox_stderr" ] ||
      [ "$(sed -n '1,$p' "$busybox_stdout")" != \
        'audit-agents-md heading validator self-test passed' ]; then
    record_failure 'outer BusyBox sh did not produce the clean self-test verdict'
  fi
fi

if [ "$failures" -ne 0 ]; then
  printf 'audit-agents-md-headings-selftest: %s failure(s)\n' "$failures" >&2
  exit 1
fi

printf 'audit-agents-md heading validator self-test passed\n'
