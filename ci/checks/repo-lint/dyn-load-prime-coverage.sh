#!/bin/sh
#
# Enforce DYN_LOAD_PRIME marker coverage over the 00_success goldens.
#
# The dyn_load_prime interpreter differential (TESTING.md § Test layers)
# is gated per case by the DYN_LOAD_PRIME marker, but the marker is
# **derived, not curated**: whether a case can ride the differential is a
# mechanical fact of the case's own tree. This gate recomputes that fact
# for every 00_success case and fails on drift, so a new golden can never
# silently skip the differential and a stale marker can never linger.
#
# A case is **eligible** exactly when:
#   1. it runs the standard harness path — `run.args` present, no `run.sh`,
#      no `run.stdin`, a root `*.pkg.kio` with a `build { ... }` block;
#   2. `expected.exit` is `0` (the differential compares a successful run);
#   3. its exact shared protocol has one of four execution modes.
#      `compile-only` stops after `load_package`; loading checks the image's
#      structural, import, host-contract, and export-contract edges but trusts
#      function-body typing. `construct-only` instantiates the exact empty-host
#      package but invokes no export. A main protocol instantiates the selected
#      exact adapter and calls `main` in the protocol's exact declaring module
#      through the loaded surface. A scripted `export-*` protocol instantiates
#      the same way, then drives its fixed surface script. Instantiation
#      compares the image's complete runtime host-function inventory against
#      the selected protocol by exact descriptor and value-group shape, and
#      checks the retained bridge requirements against the adapter. Runtime
#      execution is the semantic contract oracle; this shell lint deliberately
#      does not reimplement descriptor, reachability, or type analysis.
#
# The gate then asserts:
#   - an eligible case carries DYN_LOAD_PRIME (or the explicit
#     opt-out SKIP_DYN_LOAD_PRIME, whose content must state a
#     reason), plus a `target kio-prime { ... }` block in its manifest;
#   - an ineligible case carries neither marker (a marker there is stale;
#     a SKIP there is pointless — ineligibility is already derived);
#   - no case carries both.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

CASES_ROOT="$REPO_ROOT/test-data/goldens/00_success"
PROTOCOL_RS="$REPO_ROOT/ci/infra/kio-test-runner-rs/src/shared/protocol.rs"
MARKER=DYN_LOAD_PRIME
SKIP_MARKER=SKIP_DYN_LOAD_PRIME

# The shared registry owns every accepted name and the canonical protocol-level
# dyn-load support classification. This lint reads that bookkeeping; it does
# not infer support by inspecting a case's declarations or call graph.
protocol_constant() {
  awk -v wanted="$1" '
    $0 ~ "^pub const " wanted ": &str =" { inside = 1 }
    inside && /"/ {
      split($0, quoted, "\"")
      print quoted[2]
      exit
    }
    inside && /;/ { exit }
  ' "$PROTOCOL_RS"
}
empty_protocol=$(protocol_constant EMPTY_PROTOCOL_NAME)
known_protocols=$(awk '
  /^pub const [A-Z0-9_]*_PROTOCOL_NAME:/ { inside = 1 }
  inside && /"/ {
    split($0, quoted, "\"")
    print quoted[2]
  }
  inside && /;/ { inside = 0 }
' "$PROTOCOL_RS" | sort -u)
unsupported_protocol_constants=$(sed -n \
  '/^pub const DYN_LOAD_PRIME_UNSUPPORTED_PROTOCOL_NAMES:/,/^];/p' \
  "$PROTOCOL_RS" | sed '1d' \
  | grep -oE '[A-Z0-9_]+_PROTOCOL_NAME' | sort -u)
unsupported_protocols=$(for constant in $unsupported_protocol_constants; do
  protocol_constant "$constant"
done | sort -u)

if [ -z "$empty_protocol" ] || [ -z "$known_protocols" ] \
  || [ -z "$unsupported_protocol_constants" ] \
  || [ -z "$unsupported_protocols" ] \
  || [ "$(printf '%s\n' "$unsupported_protocol_constants" | grep -c .)" \
       -ne "$(printf '%s\n' "$unsupported_protocols" | grep -c .)" ]; then
  printf 'dyn-load-prime-coverage: cannot derive the driver execution inventory\n' >&2
  exit 1
fi

known_protocol() {
  printf '%s\n' "$known_protocols" | grep -Fxq -- "$1"
}

unsupported_protocol() {
  printf '%s\n' "$unsupported_protocols" | grep -Fxq -- "$1"
}

# The case's run.args protocol name, or empty. run.args may put the flag
# and its value on one line or on two; normalize to one token per line.
case_protocol() {
  [ -f "$1/run.args" ] || return 0
  # Both spellings the runner accepts: `--protocol NAME` and
  # `--protocol=NAME`.
  tr -s ' \t\n' '\n' <"$1/run.args" \
    | sed -n -e 's/^--protocol=//p' -e '/^--protocol$/{n;p;}' | head -n 1
}

status=0
fail() {
  printf 'dyn-load-prime-coverage: %s\n' "$1" >&2
  status=1
}

for case_dir in "$CASES_ROOT"/*/; do
  case_dir=${case_dir%/}
  name=$(basename "$case_dir")
  has_marker=0; [ -f "$case_dir/$MARKER" ] && has_marker=1
  has_skip=0;   [ -f "$case_dir/$SKIP_MARKER" ] && has_skip=1

  if [ "$has_marker" = 1 ] && [ "$has_skip" = 1 ]; then
    fail "$name carries both $MARKER and $SKIP_MARKER — pick one"
    continue
  fi

  # ---- derive eligibility ----
  eligible=1
  reason=
  pkg_source=

  if [ ! -f "$case_dir/run.args" ] || [ -e "$case_dir/run.sh" ] \
    || [ -e "$case_dir/run.stdin" ]; then
    eligible=0; reason="not on the standard run.args path"
  elif [ "$(tr -d '[:space:]' <"$case_dir/expected.exit" 2>/dev/null)" != 0 ]; then
    eligible=0; reason="expected exit is not 0"
  else
    protocol=$(case_protocol "$case_dir")
    has_protocol_flag=0
    if tr -s ' \t\n' '\n' <"$case_dir/run.args" \
      | grep -Eq '^--protocol($|=)'; then
      has_protocol_flag=1
    fi
    if [ "$has_protocol_flag" = 0 ]; then
      protocol=$empty_protocol
    fi
    if ! known_protocol "$protocol"; then
      eligible=0; reason="unknown runner protocol \`$protocol\`"
    elif unsupported_protocol "$protocol"; then
      eligible=0; reason="runner protocol \`$protocol\` has no complete dyn-load-prime adapter"
    fi
    if [ "$eligible" = 1 ]; then
      pkg=$(find "$case_dir/workdir" -maxdepth 1 -name '*.pkg.kio' -type f | head -n 1)
      if [ -z "$pkg" ] || ! pkg_source=$(cat "$pkg" 2>/dev/null) \
        || ! printf '%s\n' "$pkg_source" | grep -q '^build {'; then
        eligible=0; reason="no root manifest with a build block"
      fi
    fi
  fi

  # ---- assert marker placement ----
  if [ "$eligible" = 1 ]; then
    if [ "$has_skip" = 1 ]; then
      if [ -z "$(tr -d '[:space:]' <"$case_dir/$SKIP_MARKER")" ]; then
        fail "$name: $SKIP_MARKER must state its reason (the file is empty)"
      fi
    elif [ "$has_marker" = 0 ]; then
      fail "$name is eligible for the dyn_load_prime differential but carries no $MARKER (add the marker and a \`target kio-prime\` block, or a reasoned $SKIP_MARKER)"
    else
      if ! printf '%s\n' "$pkg_source" | grep -q '^  target kio-prime {'; then
        fail "$name carries $MARKER but its manifest declares no \`target kio-prime\` block"
      fi
    fi
  else
    if [ "$has_marker" = 1 ]; then
      fail "$name carries $MARKER but is not eligible ($reason) — the marker is stale"
    fi
    if [ "$has_skip" = 1 ]; then
      fail "$name carries $SKIP_MARKER but ineligibility is already derived ($reason) — drop the file"
    fi
  fi
done

# The numeric hosted cap is per bucket. Keep the derived differential cohort
# in its one admitted bucket so that cap remains a total-case limit.
find "$REPO_ROOT/test-data/goldens" -name "$MARKER" -type f -print |
  while IFS= read -r marker_file; do
    case $marker_file in
      "$CASES_ROOT"/*/"$MARKER") ;;
      *) printf 'dyn-load-prime-coverage: %s is outside 00_success\n' \
           "${marker_file#"$REPO_ROOT"/}" >&2
         exit 1 ;;
    esac
  done || status=1

if [ "$status" = 0 ]; then
  printf 'dyn-load-prime-coverage: OK\n'
fi
exit "$status"
