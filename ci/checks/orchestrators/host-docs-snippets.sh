#!/bin/sh
#
# Host-docs snippet checker.
#
# `kio doc` validates only the `kio` fences in `docs/hosts/<lang>.md`; the
# host-language fences (the `java` / `go` / `swift` / … code a host author
# copies) were validated by nothing. This orchestrator closes that gap.
#
# For each of the eight `docs/hosts/<lang>.md` pages it:
#
#   1. Materializes the page's own Kio example package from its Kiodoc
#      `{file}` / file-backed-harness fences (see `specs/kiodoc.md`), the
#      same file set `kio doc check` assembles.
#   2. `kio build <lang>`s that package into a scratch dir, producing the
#      real emitted facade a host compiles against.
#   3. Extracts the page's host-language code fences and compiles each
#      against that freshly-built facade — a statement-level snippet is
#      wrapped only in the imports / host scaffolding the page itself
#      shows (a documented harness preamble). A fence that fails to
#      compile, or a page whose toolchain is present but that contributes
#      zero validated host fences, fails the run.
#
# This makes docs/emitter drift a hard error: if the emitter renames a
# facade symbol the page still spells the old way, the page's host code
# stops compiling here.
#
# A few structural-shape fences (Go / Swift sum matching) illustrate a
# value shape the tiny greeter example package does not contain. They are
# validated for host-language well-formedness against a self-contained
# harness that supplies the illustrated stable aliases, constructors, and
# cases, because there is no greeter facade symbol for them to drift against.
#
# The ts.md `.d.ts` shape-illustration fence is likewise compiled
# standalone (a self-contained `.d.ts` re-declaring the facade surface to
# show its shape) rather than against the emitted greeter.d.ts; its
# facade-symbol drift is already covered by the page's other host fences,
# which do compile against the freshly-built typed skin.
#
# The python page carries a typed stub package (out/python/greeter/). A python
# fence that imports the facade (`from greeter import …`) gets a strict
# pyright type-check against the freshly-built stub on top of the syntax
# check, so a stub-symbol rename the page still spells the old way fails
# here (docs/hosts/python.md § Type-checked hosting). Every other python
# fence is syntax-checked only.
#
# `kio` is built from `kio-rs/` and snapshotted into an orchestrator-owned
# temp path, so a concurrent `cargo build` in another bucket cannot
# rewrite `target/debug/kio` mid-run (see `ai/topics/local-ci.md`). Each
# host compiler is pointed at an orchestrator-owned cache/output dir.
#
# A page's host-language toolchain may be absent on a partial-toolchain
# CI shard; that page's host-fence compile is then skipped and reported
# (the kio build still runs). JS / TS / Python / Rust are always present,
# so those pages are always fully validated. A full local toolchain
# validates all eight.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

usage() {
  cat <<EOF
Usage: sh $0 [<lang>...]

Validate the host-language code fences in docs/hosts/<lang>.md by
building each page's Kio example package and compiling the page's host
snippets against the emitted facade.

With no arguments, every page is checked. Positional <lang> arguments
(java go rust swift haskell ts js python) restrict the run to those
pages.
EOF
}

PAGES="java go rust swift haskell ts js python"
want=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    java|go|rust|swift|haskell|ts|js|python) want="$want $1" ;;
    *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done
[ -n "$want" ] && PAGES="$want"

if [ "${KIO_CI_SCHEDULE:-}" != DISABLE ] &&
   [ -z "${KIO_CI_SCHEDULER_BIN:-}" ]; then
  KIO_CI_SCHEDULER_BIN=$(sh "$REPO_ROOT/ci/schedule.sh" --prepare) || exit $?
  export KIO_CI_SCHEDULER_BIN
fi

# ---------------------------------------------------------------------------
# Build kio and snapshot the binary into an orchestrator-owned temp path.
# ---------------------------------------------------------------------------
WORK=$(mktemp -d)
# shellcheck disable=SC2317 # Invoked by the EXIT trap below.
cleanup_host_docs_snippets() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$WORK"
  exit "$cleanup_status"
}
trap cleanup_host_docs_snippets EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
KIO="$WORK/bin/kio"
mkdir -p "$WORK/bin"
export ORCHESTRATOR_TMP="$WORK"
build_corpus_tool_binary \
  kio-lsp-cli "$REPO_ROOT/kio-rs" kio "$KIO" --all-features --bins

EXTRACT_AWK="$WORK/extract.awk"
HOSTFENCES_AWK="$WORK/hostfences.awk"

cat > "$EXTRACT_AWK" <<'AWK'
# Extract the buildable Kio package files from a Kiodoc page's fences.
# A build fence is a `kio` fence carrying `{file}` (no harness=) or an
# `{@name ...}` harness reference (its body is a complete module). The
# `{harness=...}` declaration, `{variant=...}` and `{ignore}` fences are
# display / template only and are skipped. Visible and hidden fences
# both count (build fences sit at column 0). Each build fence body is
# written to raw/raw_<n>; a `raw_<n><TAB><inferred-path>` line is printed.
function classify(attrs,   a) {
  a = attrs
  if (a ~ /harness=/) return "skip"
  if (a ~ /@/) return "build"
  if (a ~ /(^|[ \t{])file([ \t}]|$)/) return "build"
  return "skip"
}
function infer_path(   i, line) {
  for (i = 0; i < n; i++) {
    line = body[i]
    sub(/^[ \t]+/, "", line)
    if (line == "" || line ~ /^\/\//) continue
    if (line ~ /^module[ \t]/) { sub(/^module[ \t]+/, "", line); sub(/[ \t]*;.*$/, "", line); return line ".kio" }
    if (line ~ /^package[ \t]/) { sub(/^package[ \t]+/, "", line); sub(/[ \t]*;.*$/, "", line); return line ".pkg.kio" }
    return ""
  }
  return ""
}
function flush(   i, f, path) {
  if (kind == "build") {
    path = infer_path()
    if (path == "") { printf("extract: build fence with no Kio file header (line %d)\n", startln) > "/dev/stderr"; errors++ }
    else { seq++; f = raw "/raw_" seq; for (i = 0; i < n; i++) print body[i] > f; close(f); print "raw_" seq "\t" path }
  }
  instate = 0; kind = ""; n = 0
}
BEGIN { instate = 0; seq = 0; errors = 0 }
instate == 1 { if ($0 ~ /^```[ \t]*$/) { flush(); next } body[n++] = $0; next }
instate == 2 { if ($0 ~ /^-->[ \t]*$/) { flush(); next } body[n++] = $0; next }
/^```kio([ \t]|{|$)/ { attrs = $0; sub(/^```kio[ \t]*/, "", attrs); kind = classify(attrs); instate = 1; n = 0; startln = NR; next }
/^<!--kio([ \t]|{)/ { attrs = $0; sub(/^<!--kio[ \t]*/, "", attrs); kind = classify(attrs); instate = 2; n = 0; startln = NR; next }
END { if (errors > 0) exit 3 }
AWK

cat > "$HOSTFENCES_AWK" <<'AWK'
# Extract every fenced block whose info string is exactly <tag> into
# raw/host_<n> (1-based, document order), one path per block to stdout.
# List-indented fences are dedented by the opening indent.
BEGIN { n = 0; instate = 0 }
instate == 1 {
  if ($0 ~ /^[ \t]*```[ \t]*$/) { close(f); instate = 0; next }
  line = $0
  if (indent > 0) sub("^ {0," indent "}", "", line)
  print line > f
  next
}
{ if ($0 ~ ("^[ \t]*```" tag "[ \t]*$")) { match($0, /^[ \t]*/); indent = RLENGTH; n++; f = raw "/host_" n; print f; instate = 1 } }
AWK

# ---------------------------------------------------------------------------
# Toolchain resolution. A missing toolchain skips that page's host-fence
# compile (partial CI shard); JS/TS/Python/Rust are always present.
# ---------------------------------------------------------------------------
resolve_tsc() {
  if command -v tsc >/dev/null 2>&1; then printf 'tsc'
  elif command -v npx >/dev/null 2>&1 && npx --no-install tsc --version >/dev/null 2>&1; then printf 'npx --no-install tsc'
  else printf ''; fi
}
resolve_pyright() {
  # `mise which` resolves the npm-pinned tool to its real binary (usable
  # from the scratch dirs, which sit outside any mise config); PATH and
  # npx are the fallbacks, mirroring ci/checks/per-case/pyright-strict.sh.
  if command -v mise >/dev/null 2>&1 && _pr=$(cd "$REPO_ROOT" && mise which pyright 2>/dev/null) && [ -n "$_pr" ]; then printf '%s' "$_pr"
  elif command -v pyright >/dev/null 2>&1; then printf 'pyright'
  elif command -v npx >/dev/null 2>&1 && npx --no-install pyright --version >/dev/null 2>&1; then printf 'npx --no-install pyright'
  else printf ''; fi
}
lang_tool() {
  case "$1" in
    java) printf 'javac' ;; go) printf 'go' ;; rust) printf 'rustc' ;;
    swift) printf 'swiftc' ;; haskell) printf 'ghc' ;; ts) resolve_tsc ;;
    js) printf 'node' ;; python) printf 'python3' ;;
  esac
}

run_compiler() {
  sh "$REPO_ROOT/ci/schedule.sh" --resource compiler -- "$@"
}

# report_fail <lang> <fence-file> <log-file>
report_fail() {
  printf 'FAIL %s: host fence did not compile:\n' "$1" >&2
  printf '  ----- fence -----\n' >&2; sed 's/^/  | /' "$2" >&2
  printf '  ----- compiler output -----\n' >&2; sed 's/^/  /' "$3" >&2
}

# first_code_line <file>: first non-blank, non-comment (// or #) line.
first_code_line() { grep -vE '^[[:space:]]*(//|#|$)' "$1" | head -1; }

# ===========================================================================
# Per-language host-fence compilers. Each: <hraw> <nfences> <artifact> <cw>.
# Writes the validated fence count to <cw>/validated. Returns non-zero if a
# fence failed to compile.
# ===========================================================================

compile_syntax() { # <hraw> <nfences> <cw> <lang> <ext> <cmd...>
  hraw=$1; nf=$2; cw=$3; lang=$4; ext=$5; shift 5
  v=0; i=1
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    cp "$f" "$cw/snippet.$ext"
    if "$@" "$cw/snippet.$ext" >"$cw/log" 2>&1; then v=$((v + 1)); else
      report_fail "$lang" "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1
    fi
  done
  printf '%s\n' "$v" > "$cw/validated"; return 0
}

compile_python() {
  hraw=$1; nf=$2; art=$3; cw=$4
  PYRIGHT=$(resolve_pyright)
  if [ -z "$PYRIGHT" ]; then
    printf '  SKIP typed-fence pyright check: pyright not installed (syntax check still runs)\n'
  fi
  v=0; i=1
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    cp "$f" "$cw/snippet.py"
    if ! python3 -m py_compile "$cw/snippet.py" >"$cw/log" 2>&1; then
      report_fail python "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1
    fi
    # A fence that imports the typed facade additionally gets a strict
    # pyright pass against the freshly-built stub package (the greeter/
    # typed view shadows greeter.py), so a stub-symbol drift the
    # page still spells the old way fails here. Strict mode + the
    # backend's target version mirror ci/checks/per-case/pyright-strict.sh.
    if [ -n "$PYRIGHT" ] && grep -qE '^[[:space:]]*(from[[:space:]]+greeter([.[:space:]])|import[[:space:]]+greeter([[:space:]]|$))' "$f"; then
      d="$cw/typed"; rm -rf "$d"; mkdir -p "$d"
      cp -R "$art"/. "$d/"
      cp "$f" "$d/snippet.py"
      printf '{ "typeCheckingMode": "strict", "pythonVersion": "3.10" }\n' > "$d/pyrightconfig.json"
      # pyright roots config discovery and local-module resolution at its
      # cwd, so run from inside the typed dir (as pyright-strict.sh does).
      if ! ( cd "$d" && $PYRIGHT snippet.py ) >"$cw/log" 2>&1; then
        printf 'FAIL python: typed host fence failed strict pyright against the built stub:\n' >&2
        printf '  ----- fence -----\n' >&2; sed 's/^/  | /' "$f" >&2
        printf '  ----- pyright output -----\n' >&2; sed 's/^/  /' "$cw/log" >&2
        printf '%s\n' "$v" > "$cw/validated"; return 1
      fi
    fi
    v=$((v + 1))
  done
  printf '%s\n' "$v" > "$cw/validated"; return 0
}

compile_rust() {
  hraw=$1; nf=$2; art=$3; cw=$4
  if ! run_compiler rustc "$art/src/lib.rs" --crate-type=rlib \
       --crate-name=greeter --edition=2024 -C debuginfo=0 --out-dir "$cw" \
       >"$cw/rlib.log" 2>&1; then
    printf 'FAIL rust: emitted crate did not compile to rlib:\n' >&2; sed 's/^/  /' "$cw/rlib.log" >&2
    printf '0\n' > "$cw/validated"; return 1
  fi
  rlib="$cw/libgreeter.rlib"
  v=0; i=1
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    if grep -q 'fn main' "$f"; then                       # complete program
      cp "$f" "$cw/d.rs"; ct=bin
    else
      case "$(first_code_line "$f")" in
        use\ *|impl\ *|struct\ *|enum\ *|trait\ *|pub\ *|fn\ *|type\ *|const\ *|mod\ *|"#["*)
          cp "$f" "$cw/d.rs"; ct=lib ;;                    # top-level items
        *)                                                 # statements → harness
          { printf 'use greeter::host::GreeterHost;\n#[derive(Clone)] struct MyHost;\n'
            printf 'impl GreeterHost for MyHost { type greeter__String = String; fn greeter__print(&self, s: Self::greeter__String) { let _ = s; } }\n'
            printf 'fn main() {\n'
            grep -q 'let pkg' "$f" || printf 'let pkg = greeter::create_greeter(MyHost);\nlet _ = &pkg;\n'
            cat "$f"; printf '\n}\n'; } > "$cw/d.rs"; ct=bin ;;
      esac
    fi
    if run_compiler rustc "$cw/d.rs" --extern greeter="$rlib" \
         --edition=2024 -C debuginfo=0 --crate-type=$ct --emit=metadata \
         --out-dir "$cw" >"$cw/log" 2>&1; then v=$((v + 1))
    else report_fail rust "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
  done
  printf '%s\n' "$v" > "$cw/validated"; return 0
}

compile_go() {
  hraw=$1; nf=$2; art=$3; cw=$4
  # Scratch two-module layout: greeter (the emitted package, given a
  # synthesized go.mod so it can be a replace target — the emitter emits
  # no go.mod) and host (imports it, exactly as the page's go.mod shows).
  mkdir -p "$cw/greeter" "$cw/host"
  cp "$art"/*.go "$cw/greeter/"
  printf 'module greeter\n\ngo 1.26\n' > "$cw/greeter/go.mod"
  printf 'module ffihost\n\ngo 1.26\n\nrequire greeter v0.0.0\n\nreplace greeter => ../greeter\n' > "$cw/host/go.mod"
  export GOFLAGS=-mod=mod GOPROXY=off GO111MODULE=on GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local
  export TEST_TELEMETRY_DIR="$cw/.telemetry"
  export GOCACHE="$cw/gocache" GOMODCACHE="$cw/gomodcache"
  go_build() {
    ( cd "$cw/host" && run_compiler go build . ) >"$cw/log" 2>&1
  }
  v=0; i=1; frags=""
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    if grep -qE 'Env_Api__(pair|choice)_ret' "$f"; then    # structural illustration
      go_illustration "$f" "$cw" || { report_fail go "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; }
      v=$((v + 1))
    elif grep -q 'func main' "$f"; then                   # complete program
      cp "$f" "$cw/host/main.go"
      if go_build; then v=$((v + 1)); else report_fail go "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    else frags="$frags $f"; fi
  done
  if [ -n "$frags" ]; then                                # accumulate walkthrough
    # shellcheck disable=SC2086 # $frags is a deliberate word-split file list
    assemble_go $frags > "$cw/host/main.go"
    if go_build; then for f in $frags; do v=$((v + 1)); done
    else report_fail go "$cw/host/main.go" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
  fi
  printf '%s\n' "$v" > "$cw/validated"; return 0
}
assemble_go() {
  tops=""; stmts=""
  for f in "$@"; do
    if grep -qE '^[[:space:]]*(package|import|func|type|var|const)[[:space:]]' "$f"; then tops="$tops $f"; else stmts="$stmts $f"; fi
  done
  # shellcheck disable=SC2086 # $tops is a deliberate word-split file list
  grep -qhE '^[[:space:]]*package[[:space:]]' $tops 2>/dev/null || printf 'package main\n\n'
  for f in $tops; do cat "$f"; printf '\n'; done
  printf 'func main() {\n'
  for f in $stmts; do cat "$f"; printf '\n'; done
  printf '}\n'
}
go_illustration() { # <fence> <cw> : validate the exact product/sum alias API
  f=$1; cw=$2; d="$cw/illus"; rm -rf "$d"; mkdir -p "$d/greeter"
  printf 'module greeter\n\ngo 1.26\n' > "$d/greeter/go.mod"
  printf '%s\n' 'package greeter

type Product[T0, T1 any] struct {
	F0 T0
	F1 T1
}

type Env_Api__pair_ret = Product[int32, string]

type kioCase interface {
	kioCaseMarker()
}

type kioRow[R any] interface {
	zeroCase(R) kioCase
}

type KioSum[R kioRow[R]] struct {
	stored kioCase
}

func (value KioSum[R]) Case() kioCase {
	if value.stored != nil {
		return value.stored
	}
	var row R
	return row.zeroCase(row)
}

type choiceRow struct{}

func (choiceRow) zeroCase(choiceRow) kioCase {
	return choiceCase0{}
}

type Env_Api__choice_ret = KioSum[choiceRow]

type Env_Api__choice_ret_0 = interface {
	kioCase
	Value() string
}

type choiceCase0 struct {
	value string
}

func (choiceCase0) kioCaseMarker() {}

func (value choiceCase0) Value() string {
	return value.value
}

func NewEnv_Api__choice_ret_0(value string) Env_Api__choice_ret {
	return KioSum[choiceRow]{stored: choiceCase0{value: value}}
}

type Env_Api__choice_ret_1 = interface {
	kioCase
	Value() int32
}

type choiceCase1 struct {
	value int32
}

func (choiceCase1) kioCaseMarker() {}

func (value choiceCase1) Value() int32 {
	return value.value
}

func NewEnv_Api__choice_ret_1(value int32) Env_Api__choice_ret {
	return KioSum[choiceRow]{stored: choiceCase1{value: value}}
}' > "$d/greeter/shapes.go"
  printf 'module ffi_illus\n\ngo 1.26\n\nrequire greeter v0.0.0\n\nreplace greeter => ./greeter\n' > "$d/go.mod"
  { printf 'package main\n\nimport "greeter"\n\nfunc use(x any) { _ = x }\n\nfunc main() {\n'; cat "$f"; printf '}\n'; } > "$d/main.go"
  ( cd "$d" && run_compiler go build . ) >"$cw/log" 2>&1
}

compile_java() {
  hraw=$1; nf=$2; art=$3; cw=$4
  mkdir -p "$cw/greeter" "$cw/classes"
  cp "$art/greeter"/*.java "$cw/greeter/"
  javac_it() {
    ( cd "$cw" && run_compiler javac -d classes greeter/*.java "$1" ) \
      >"$cw/log" 2>&1
  }
  v=0; i=1; frags=""
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    if grep -qE '(static[[:space:]]+)?void[[:space:]]+main[[:space:]]*\(' "$f"; then   # complete program
      pub=$(grep -oE 'public[[:space:]]+(final[[:space:]]+)?class[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' "$f" | grep -oE '[A-Za-z_][A-Za-z0-9_]*$' | head -1)
      pub=${pub:-Main}; cp "$f" "$cw/$pub.java"
      if javac_it "$cw/$pub.java"; then v=$((v + 1)); else report_fail java "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    else frags="$frags $f"; fi   # host-impl class + instantiate steps → one walkthrough
  done
  if [ -n "$frags" ]; then
    # shellcheck disable=SC2086 # $frags is a deliberate word-split file list
    assemble_java $frags > "$cw/__W.java"
    if javac_it "$cw/__W.java"; then for f in $frags; do v=$((v + 1)); done
    else report_fail java "$cw/__W.java" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
  fi
  printf '%s\n' "$v" > "$cw/validated"; return 0
}
assemble_java() {
  for f in "$@"; do grep -E '^[[:space:]]*import[[:space:]]' "$f"; done | sort -u
  for f in "$@"; do grep -qE '(^|[[:space:]])(class|interface|enum)[[:space:]]' "$f" && { grep -vE '^[[:space:]]*import[[:space:]]' "$f"; printf '\n'; }; done
  printf 'public class __W {\n  public static void main(String[] __a) {\n'
  for f in "$@"; do grep -qE '(^|[[:space:]])(class|interface|enum)[[:space:]]' "$f" || { grep -vE '^[[:space:]]*import[[:space:]]' "$f"; printf '\n'; }; done
  printf '  }\n}\n'
}

compile_swift() {
  hraw=$1; nf=$2; art=$3; cw=$4
  mod=$(head -1 "$art/pkg.swift" | sed 's|^//[[:space:]]*kio-swift-module:[[:space:]]*||')
  [ -n "$mod" ] || mod=Greeter
  mkdir -p "$cw/mc"
  if ! run_compiler swiftc -emit-module -module-name "$mod" \
       -emit-module-path "$cw/$mod.swiftmodule" -module-cache-path "$cw/mc" \
       "$art"/*.swift >"$cw/mod.log" 2>&1; then
    printf 'FAIL swift: emitted module did not build:\n' >&2; sed 's/^/  /' "$cw/mod.log" >&2
    printf '0\n' > "$cw/validated"; return 1
  fi
  sc() {
    run_compiler swiftc -typecheck -I "$cw" -module-name hostcheck \
      -module-cache-path "$cw/mc" "$1" >"$cw/log" 2>&1
  }
  v=0; i=1; frags=""
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    if grep -q 'switch result' "$f"; then                 # structural illustration
      { printf 'enum __Sum { case _0(Swift.String); case _1(Swift.Int) }\nfunc use<T>(_ x: T) {}\nlet result: __Sum = ._0("x")\n'; cat "$f"; } > "$cw/i.swift"
      if sc "$cw/i.swift"; then v=$((v + 1)); else report_fail swift "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    elif grep -q 'struct ' "$f" && grep -q 'createGreeter' "$f"; then   # complete program
      cp "$f" "$cw/p.swift"
      if sc "$cw/p.swift"; then v=$((v + 1)); else report_fail swift "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    else frags="$frags $f"; fi
  done
  if [ -n "$frags" ]; then
    : > "$cw/w.swift"; for f in $frags; do cat "$f" >> "$cw/w.swift"; printf '\n' >> "$cw/w.swift"; done
    if sc "$cw/w.swift"; then for f in $frags; do v=$((v + 1)); done
    else report_fail swift "$cw/w.swift" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
  fi
  printf '%s\n' "$v" > "$cw/validated"; return 0
}

compile_haskell() {
  hraw=$1; nf=$2; art=$3; cw=$4
  mkdir -p "$cw/hi"
  ghc_it() {
    run_compiler ghc -fno-code -i"$art" -outputdir "$cw/hi" "$1" \
      >"$cw/log" 2>&1
  }
  v=0; i=1; frags=""
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    if grep -q 'createGreeter' "$f" && grep -qE '^main[[:space:]]*::' "$f"; then   # complete program
      cp "$f" "$cw/Main.hs"
      if ghc_it "$cw/Main.hs"; then v=$((v + 1)); else report_fail haskell "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    else frags="$frags $f"; fi
  done
  if [ -n "$frags" ]; then
    # shellcheck disable=SC2086 # $frags is a deliberate word-split file list
    assemble_haskell $frags > "$cw/Walk.hs"
    if ghc_it "$cw/Walk.hs"; then for f in $frags; do v=$((v + 1)); done
    else report_fail haskell "$cw/Walk.hs" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
  fi
  printf '%s\n' "$v" > "$cw/validated"; return 0
}
assemble_haskell() {
  # imports first, then non-main top-level bindings, then a synthesized
  # `main = do` that threads the do-let fragment(s) and the main action.
  for f in "$@"; do grep -E '^import[[:space:]]' "$f"; done
  for f in "$@"; do
    grep -qE '^import[[:space:]]' "$f" && ! grep -qvE '^import[[:space:]]|^[[:space:]]*$' "$f" && continue  # pure-imports fragment
    grep -qE '^main[[:space:]]*::|^main[[:space:]]*=' "$f" && continue                                       # the main fragment
    grep -qE '^[[:space:]]*let[[:space:]]' "$f" && continue                                                  # do-let fragment
    grep -vE '^import[[:space:]]' "$f"; printf '\n'                                                          # binding (e.g. stubHost)
  done
  printf 'main :: IO ()\nmain = do\n'
  for f in "$@"; do grep -E '^[[:space:]]*let[[:space:]]' "$f" | sed 's/^/  /'; done
  for f in "$@"; do
    grep -qE '^main[[:space:]]*::|^main[[:space:]]*=' "$f" || continue
    # the main action: the body after `main = `, indented into the do block
    sed -n 's/^main[[:space:]]*=[[:space:]]*//p' "$f" | sed 's/^/  /'
  done
}

compile_ts() {
  hraw=$1; nf=$2; art=$3; cw=$4
  TSC=$(resolve_tsc)
  # Mirror the emitted tree under out/ts/ so the page's own
  # `import './out/ts/greeter.js'` resolves the typed skin, and add a
  # documented harness preamble: the Node `process` global the page
  # assumes ("A Node host runs it").
  mkdir -p "$cw/out/ts"
  cp "$art"/*.js "$art"/*.d.ts "$cw/out/ts/" 2>/dev/null || true
  printf 'declare const process: { stdout: { write(s: string): unknown } };\n' > "$cw/__harness.d.ts"
  tsc_it() { ( cd "$cw" && $TSC --strict --noEmit --skipLibCheck "$@" ) >"$cw/log" 2>&1; }
  v=0; i=1; frags=""
  while [ "$i" -le "$nf" ]; do
    f="$hraw/host_$i"; i=$((i + 1))
    if grep -q 'export interface' "$f" && grep -q 'export function createGreeter' "$f"; then   # .d.ts shape illustration
      cp "$f" "$cw/skin.d.ts"
      if tsc_it skin.d.ts; then v=$((v + 1)); else report_fail ts "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    elif grep -qE 'createGreeter\(host\)' "$f" && grep -q 'import ' "$f"; then                  # complete program
      cp "$f" "$cw/p.ts"
      if tsc_it __harness.d.ts p.ts; then v=$((v + 1)); else report_fail ts "$f" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
    else frags="$frags $f"; fi
  done
  if [ -n "$frags" ]; then
    { # If no fragment imports the facade, add the import the page shows.
      # shellcheck disable=SC2086 # $frags is a deliberate word-split file list
      grep -hqE "^import .*greeter\.js" $frags || printf "import { createGreeter, Greeter, GreeterHost } from './out/ts/greeter.js';\n"
      for f in $frags; do cat "$f"; printf '\n'; done
    } > "$cw/w.ts"
    if tsc_it __harness.d.ts w.ts; then for f in $frags; do v=$((v + 1)); done
    else report_fail ts "$cw/w.ts" "$cw/log"; printf '%s\n' "$v" > "$cw/validated"; return 1; fi
  fi
  printf '%s\n' "$v" > "$cw/validated"; return 0
}

compile_page() {
  page=$1; hraw=$2; nf=$3; art=$4; cw=$5
  case "$page" in
    rust) compile_rust "$hraw" "$nf" "$art" "$cw" ;;
    go) compile_go "$hraw" "$nf" "$art" "$cw" ;;
    java) compile_java "$hraw" "$nf" "$art" "$cw" ;;
    swift) compile_swift "$hraw" "$nf" "$art" "$cw" ;;
    haskell) compile_haskell "$hraw" "$nf" "$art" "$cw" ;;
    ts) compile_ts "$hraw" "$nf" "$art" "$cw" ;;
    js) compile_syntax "$hraw" "$nf" "$cw" js mjs node --check ;;
    python) compile_python "$hraw" "$nf" "$art" "$cw" ;;
  esac
}

# ---------------------------------------------------------------------------
# Main per-page loop.
# ---------------------------------------------------------------------------
fail=0; skipped=0; SUMMARY=""
for page in $PAGES; do
  printf '\n========== host-docs-snippets: %s ==========\n' "$page"
  pagemd="$REPO_ROOT/docs/hosts/$page.md"
  [ -f "$pagemd" ] || { printf 'FAIL %s: no docs/hosts/%s.md\n' "$page" "$page" >&2; fail=1; continue; }

  pw="$WORK/$page"; mkdir -p "$pw"
  raw="$pw/raw"; mkdir -p "$raw"
  pkg="$pw/pkg"; mkdir -p "$pkg"

  # 1. Materialize the example package from the page's kio fences.
  if ! awk -v raw="$raw" -f "$EXTRACT_AWK" "$pagemd" >"$pw/manifest" 2>"$pw/extract.err"; then
    printf 'FAIL %s: fence extraction error:\n' "$page" >&2; sed 's/^/  /' "$pw/extract.err" >&2; fail=1; continue
  fi
  if [ ! -s "$pw/manifest" ]; then
    printf 'FAIL %s: no buildable kio fences (page does not assemble into a package)\n' "$page" >&2; fail=1; continue
  fi
  while IFS="$(printf '\t')" read -r rawf path; do
    mkdir -p "$pkg/$(dirname "$path")"; cp "$raw/$rawf" "$pkg/$path"
  done < "$pw/manifest"
  printf '  materialized: %s\n' "$(cd "$pkg" && find . -name '*.kio' | sort | tr '\n' ' ')"

  # 2. Build the package for this target.
  if ! ( cd "$pkg" && run_compiler "$KIO" build "$page" ) >"$pw/build.log" 2>&1; then
    printf 'FAIL %s: kio build %s failed:\n' "$page" "$page" >&2; sed 's/^/  /' "$pw/build.log" >&2; fail=1; continue
  fi
  artifact="$pkg/out/$page"
  printf '  built: out/%s\n' "$page"

  # 3. Extract host-language fences.
  hraw="$pw/host"; mkdir -p "$hraw"
  awk -v raw="$hraw" -v tag="$page" -f "$HOSTFENCES_AWK" "$pagemd" >"$pw/hostlist" 2>/dev/null || true
  nfences=$(grep -c 'host_' "$pw/hostlist" 2>/dev/null || printf '0')
  if [ "$nfences" = 0 ]; then
    printf 'FAIL %s: no %s host-language fences on the page\n' "$page" "$page" >&2; fail=1; continue
  fi

  tool=$(lang_tool "$page")
  if [ -z "$tool" ] || ! command -v "${tool%% *}" >/dev/null 2>&1; then
    printf '  SKIP host-fence compile: toolchain %s not installed (%s fences)\n' "${tool:-<none>}" "$nfences"
    skipped=$((skipped + 1))
    SUMMARY="$SUMMARY\n  $page: built ok, host-fence compile SKIPPED ($nfences fences, no ${tool:-toolchain})"
    continue
  fi

  cw="$pw/compile"; mkdir -p "$cw"
  if compile_page "$page" "$hraw" "$nfences" "$artifact" "$cw"; then
    validated=$(cat "$cw/validated" 2>/dev/null || printf '0')
    printf '  validated %s/%s host fence(s)\n' "$validated" "$nfences"
    if [ "$validated" = 0 ]; then
      printf 'FAIL %s: zero validated host fences (no-silent-skip)\n' "$page" >&2; fail=1
    fi
    SUMMARY="$SUMMARY\n  $page: $validated/$nfences host fences compiled"
  else
    fail=1
    SUMMARY="$SUMMARY\n  $page: HOST FENCE COMPILE FAILED"
  fi
done

printf '\n========== host-docs-snippets: summary =========='
printf '%b\n' "$SUMMARY"
[ "$skipped" != 0 ] && printf '(%s page(s) had their host-fence compile skipped for an absent toolchain)\n' "$skipped"

if [ "$fail" != 0 ]; then printf '\nhost-docs-snippets: FAILED\n' >&2; exit 1; fi
printf '\nhost-docs-snippets: PASSED\n'
