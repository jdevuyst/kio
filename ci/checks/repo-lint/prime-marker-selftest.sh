#!/bin/sh
# Exercise source discovery, bounded child work and marker verdicts without
# building a compiler. Real tools handle the fixtures; wrappers inject errors
# and model Git's Unicode argument normalization.
set -eu
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)
check=$REPO_ROOT/ci/checks/per-case/prime-marker.sh
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
REAL_GIT=$(command -v git)
REAL_FIND=$(command -v find)
REAL_AWK=$(command -v awk)
export REAL_GIT REAL_FIND REAL_AWK
mkdir -p "$scratch/bin" "$scratch/tracked/workdir/out" \
  "$scratch/generated/workdir" "$scratch/empty/workdir" "$scratch/absent" \
  "$scratch/tracked/draft/workdir" "$scratch/tmp space\\literal"
mkdir -p "$scratch/trivia/workdir"
TRACE=$scratch/trace
mkdir "$TRACE"
export TRACE

cat > "$scratch/bin/git" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$TRACE/git"
case "$*" in
  *'ls-files -- .')
    [ "${FAULT:-}" != git-list ] || exit 128 ;;
  *'--error-unmatch'*)
    [ "${FAULT:-}" != git-lookup ] || exit 128 ;;
esac
if [ "${MODEL_PRECOMPOSE:-0}" = 1 ] && [ "$#" -eq 4 ] &&
  [ "$1" = ls-files ] && [ "$2" = --error-unmatch ] &&
  [ "$4" = ":(literal)$NFD" ]; then
  # Model only Git's macOS literal-argument Unicode normalization.
  exec "$REAL_GIT" ls-files --error-unmatch -- ":(literal)$NFC"
fi
exec "$REAL_GIT" "$@"
EOF
cat > "$scratch/bin/find" <<'EOF'
#!/bin/sh
case "${FAULT:-}" in
  traversal) printf 'workdir/plain.kio\n'; exit 1 ;;
  read) printf 'workdir/missing.kio\n'; exit 0 ;;
esac
"$REAL_FIND" "$@" > "$TRACE/found" || exit $?
cat "$TRACE/found"
EOF
cat > "$scratch/bin/awk" <<'EOF'
#!/bin/sh
printf 'awk\n' >> "$TRACE/awk"
[ "${FAULT:-}" != classify ] || exit 2
exec "$REAL_AWK" "$@"
EOF
cat > "$scratch/bin/verify" <<'EOF'
#!/bin/sh
test "$#" -eq 1 || exit 2
printf '%s\n' "$1" >> "$TRACE/verified"
[ "$1" != "${REJECT_FILE:-}" ] || exit 1
[ "${REJECT:-0}" = 0 ]
EOF
chmod +x "$scratch/bin/git" "$scratch/bin/find" "$scratch/bin/awk" "$scratch/bin/verify"

# Header classification is intentionally lexical: a later module line and a
# leading block comment are not selected. Files after a blank/line comment are.
printf '\n  // heading\n  module plain;\n' > "$scratch/tracked/workdir/plain.kio"
printf 'module spaced;\n' > "$scratch/tracked/workdir/space [x].kio"
printf 'not_a_module\nmodule later;\n' > "$scratch/tracked/workdir/nonmodule.kio"
printf '/* heading */\nmodule later;\n' > "$scratch/tracked/workdir/block.kio"
printf '// comment only\n\n' > "$scratch/tracked/workdir/empty.kio"
printf 'module excluded;\n' > "$scratch/tracked/workdir/skip.pkg.kio"
printf 'module excluded;\n' > "$scratch/tracked/workdir/out/skip.kio"
i=0
while [ "$i" -lt 32 ]; do
  printf 'module wide;\n' > "$scratch/tracked/workdir/wide$i.kio"
  i=$((i + 1))
done
printf 'module generated;\n' > "$scratch/generated/workdir/source.kio"
printf 'module draft;\n' > "$scratch/tracked/draft/workdir/draft.kio"
ln -s plain.kio "$scratch/tracked/workdir/linked.kio"
ln -s ../../generated/workdir "$scratch/tracked/workdir/linked-dir"
"$REAL_GIT" -C "$scratch/tracked" init -q
"$REAL_GIT" -C "$scratch/tracked" add -- workdir
printf 'module untracked;\n' > "$scratch/tracked/workdir/extra.kio"

fail() { printf 'prime-marker-selftest: %s\n' "$*" >&2; exit 1; }
run_check() {
  case_root=$1 expected=$2
  : > "$TRACE/git"
  : > "$TRACE/awk"
  : > "$TRACE/verified"
  : > "$TRACE/found"
  actual=0
  (cd "$case_root" && PATH="$scratch/bin:$PATH" \
    KIO_PRIME_CHECK_BIN="$scratch/bin/verify" \
    KIO_TEST_UPDATE="${UPDATE:-0}" sh "$check") \
    > "$TRACE/stdout" 2> "$TRACE/stderr" || actual=$?
  [ "$actual" -eq "$expected" ] || {
    cat "$TRACE/stderr" >&2
    fail "expected exit $expected, got $actual"
  }
}
expect_verified() {
  printf '%s\n' "$@" > "$scratch/expected"
  cmp "$scratch/expected" "$TRACE/verified" || fail 'verifier input/order changed'
}
expect_no_verifier() {
  [ ! -s "$TRACE/verified" ] || fail 'verifier ran after discovery failure'
}
expect_fault() {
  FAULT=$1
  export FAULT
  UPDATE=1
  run_check "$2" 1
  expect_no_verifier
  [ -f "$2/IS_KIO_PRIME" ] || fail 'discovery failure changed marker'
  grep "$3" "$TRACE/stderr" >/dev/null || fail 'missing discovery diagnostic'
  unset FAULT UPDATE
}

# Each admitted separator must reach the verifier, including when it is the
# only module. A same-prefix identifier is not the module keyword.
for separator in tab newline comment; do
  case "$separator" in
    tab) printf 'module\ttrivia;\n' ;;
    newline) printf 'module\ntrivia;\n' ;;
    comment) printf 'module// heading\ntrivia;\n' ;;
  esac > "$scratch/trivia/workdir/trivia.kio"
  rm -f "$scratch/trivia/IS_KIO_PRIME"
  run_check "$scratch/trivia" 1
  expect_verified workdir/trivia.kio
  UPDATE=1
  run_check "$scratch/trivia" 0
  unset UPDATE
  [ -f "$scratch/trivia/IS_KIO_PRIME" ] || fail 'trivia module did not create marker'
  run_check "$scratch/trivia" 0
  expect_verified workdir/trivia.kio
  REJECT_FILE=workdir/trivia.kio
  export REJECT_FILE
  run_check "$scratch/trivia" 1
  UPDATE=1
  run_check "$scratch/trivia" 0
  unset UPDATE REJECT_FILE
  [ ! -f "$scratch/trivia/IS_KIO_PRIME" ] || fail 'rejected trivia module retained marker'
done
printf 'module regular;\n' > "$scratch/trivia/workdir/regular.kio"
printf 'module_name unrelated;\n' > "$scratch/trivia/workdir/nonmodule.kio"
printf 'module\ttrivia;\n' > "$scratch/trivia/workdir/trivia.kio"
: > "$scratch/trivia/IS_KIO_PRIME"
run_check "$scratch/trivia" 0
LC_ALL=C sort "$TRACE/verified" > "$scratch/actual"
printf 'workdir/regular.kio\nworkdir/trivia.kio\n' > "$scratch/expected"
cmp "$scratch/expected" "$scratch/actual" || fail 'mixed-module selection changed'
REJECT_FILE=workdir/trivia.kio
export REJECT_FILE
run_check "$scratch/trivia" 1
grep -Fx workdir/trivia.kio "$TRACE/verified" >/dev/null || fail 'mixed rejection was not verified'
UPDATE=1
run_check "$scratch/trivia" 0
unset UPDATE REJECT_FILE
[ ! -f "$scratch/trivia/IS_KIO_PRIME" ] || fail 'mixed rejection retained marker'

: > "$scratch/tracked/IS_KIO_PRIME"
run_check "$scratch/tracked" 0
if [ -s "$TRACE/stdout" ] || [ -s "$TRACE/stderr" ]; then
  fail 'unexpected successful output'
fi
[ "$(wc -l < "$TRACE/git" | tr -d ' ')" -eq 2 ] || fail 'per-file Git queries returned'
[ "$(wc -l < "$TRACE/awk" | tr -d ' ')" -eq 1 ] || fail 'header classification is not one pass'
# Expected identities come from the fixture, not the production header filter.
# shellcheck disable=SC2016 # This is an awk program, not shell interpolation.
"$REAL_AWK" '
  /^workdir\/wide[0-9]+\.kio$/ || $0 == "workdir/plain.kio" ||
  $0 == "workdir/space [x].kio" || $0 == "workdir/linked.kio" { print }
' "$TRACE/found" > "$scratch/expected"
cmp "$scratch/expected" "$TRACE/verified" || fail 'tracked membership or find order changed'
[ "$(wc -l < "$TRACE/verified" | tr -d ' ')" -eq 35 ] || fail 'wide fixture lost modules'

# A temporary root containing literal backslashes must not become awk escapes.
saved_tmp=${TMPDIR:-}
TMPDIR=$scratch/tmp\ space\\literal
export TMPDIR
run_check "$scratch/tracked" 0
if [ -n "$saved_tmp" ]; then TMPDIR=$saved_tmp; export TMPDIR; else unset TMPDIR; fi

# An awk filename operand containing '=' would instead be an assignment.
# The relative temporary directory is still inside this test's scratch root.
mkdir "$scratch/tracked/local=tmp"
TMPDIR=local=tmp
export TMPDIR
run_check "$scratch/tracked" 0 </dev/null
[ "$(wc -l < "$TRACE/verified" | tr -d ' ')" -eq 35 ] || fail 'relative TMPDIR lost modules'
if [ -n "$saved_tmp" ]; then TMPDIR=$saved_tmp; export TMPDIR; else unset TMPDIR; fi

run_check "$scratch/generated" 1
expect_verified workdir/source.kio
UPDATE=1
run_check "$scratch/generated" 0
unset UPDATE
[ -f "$scratch/generated/IS_KIO_PRIME" ] || fail 'update did not create marker'
: > "$scratch/tracked/draft/IS_KIO_PRIME"
run_check "$scratch/tracked/draft" 0
expect_verified workdir/draft.kio
run_check "$scratch/empty" 0
expect_no_verifier
: > "$scratch/empty/IS_KIO_PRIME"
run_check "$scratch/empty" 1
grep 'no regular-module' "$TRACE/stderr" >/dev/null || fail 'empty marker diagnostic changed'
run_check "$scratch/absent" 0
expect_no_verifier

REJECT=1
export REJECT
run_check "$scratch/tracked" 1
[ "$(wc -l < "$TRACE/verified" | tr -d ' ')" -eq 1 ] || fail 'verifier did not stop at first rejection'
UPDATE=1
run_check "$scratch/tracked" 0
unset UPDATE REJECT
[ ! -f "$scratch/tracked/IS_KIO_PRIME" ] || fail 'update did not remove marker'
: > "$scratch/tracked/IS_KIO_PRIME"

expect_fault git-list "$scratch/tracked" 'cannot list tracked'
expect_fault traversal "$scratch/tracked" 'cannot discover'
expect_fault read "$scratch/generated" 'cannot read'
expect_fault classify "$scratch/tracked" 'cannot classify'

# C-quoted Git records take exact literal queries, not a pathname decoder.
printf 'module quoted;\n' > "$scratch/tracked/workdir/quote\"name.kio"
printf 'module slash;\n' > "$scratch/tracked/workdir/back\\slash.kio"
"$REAL_GIT" -C "$scratch/tracked" add -- 'workdir/quote"name.kio' 'workdir/back\slash.kio'
run_check "$scratch/tracked" 0
grep -F 'workdir/quote"name.kio' "$TRACE/verified" >/dev/null || fail 'quoted path lost'
grep -F 'workdir/back\slash.kio' "$TRACE/verified" >/dev/null || fail 'backslash path lost'
grep -- '--error-unmatch' "$TRACE/git" >/dev/null || fail 'quoted-path literal lookup did not fire'
[ "$(wc -l < "$TRACE/verified" | tr -d ' ')" -eq 37 ] || fail 'quoted fallback membership changed'
# shellcheck disable=SC2016 # Literal fixture identities in an awk program.
"$REAL_AWK" '
  /^workdir\/wide[0-9]+\.kio$/ || $0 == "workdir/plain.kio" ||
  $0 == "workdir/space [x].kio" || $0 == "workdir/linked.kio" ||
  $0 == "workdir/quote\"name.kio" || $0 == "workdir/back\\slash.kio" { print }
' "$TRACE/found" > "$scratch/expected"
cmp "$scratch/expected" "$TRACE/verified" || fail 'quoted-path order changed'
expect_fault git-lookup "$scratch/tracked" 'cannot check tracked source'

# Keep a composed index name and a decomposed filesystem name. An ASCII
# intermediate forces the stored spelling even on normalization-insensitive
# filesystems. The wrapper models only Git's argument precomposition.
mkdir -p "$scratch/normalization/workdir"
NFC=$(printf 'workdir/caf\303\251.kio')
NFD=$(printf 'workdir/cafe\314\201.kio')
export NFC NFD
printf 'module cafe;\n' > "$scratch/normalization/$NFC"
"$REAL_GIT" -C "$scratch/normalization" init -q
"$REAL_GIT" -C "$scratch/normalization" config core.precomposeUnicode true
"$REAL_GIT" -C "$scratch/normalization" config core.quotePath false
"$REAL_GIT" -C "$scratch/normalization" add -- "$NFC"
mv "$scratch/normalization/$NFC" "$scratch/normalization/workdir/rename.kio"
mv "$scratch/normalization/workdir/rename.kio" "$scratch/normalization/$NFD"
: > "$scratch/normalization/IS_KIO_PRIME"
MODEL_PRECOMPOSE=1
export MODEL_PRECOMPOSE
run_check "$scratch/normalization" 0
expect_verified "$NFD"
grep -- '--error-unmatch' "$TRACE/git" >/dev/null || fail 'normalization fallback did not fire'
unset MODEL_PRECOMPOSE NFC NFD

# Exercise available caller locales without changing the check's environment.
mkdir -p "$scratch/locale/workdir"
printf '\342\200\203module spaced;\n' > "$scratch/locale/workdir/unicode.kio"
for locale_name in C C.UTF-8; do
  if LC_ALL="$locale_name" locale charmap >/dev/null 2>&1; then
    LC_ALL=$locale_name
    export LC_ALL
    # POSIX sed supplies the independent character-class oracle used by the
    # original first-line filter, including non-ASCII leading whitespace.
    first_line=$(sed -n 's/^[[:space:]]*//; p; q' "$scratch/locale/workdir/unicode.kio")
    rm -f "$scratch/locale/IS_KIO_PRIME"
    case "$first_line" in 'module '*) : > "$scratch/locale/IS_KIO_PRIME" ;; esac
    run_check "$scratch/locale" 0
    case "$first_line" in
      'module '*) expect_verified workdir/unicode.kio ;;
      *) expect_no_verifier ;;
    esac
  fi
done
printf 'prime-marker-selftest: discovery, bounded work, errors and marker semantics passed\n'
