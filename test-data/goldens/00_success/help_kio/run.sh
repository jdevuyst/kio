#!/bin/sh
# Conformance-check every `kio` `--help` surface against the
# audit-cli-output §C2 / §C4 contract (ai/skills/audit-cli-output/SKILL.md).
#
# The subcommand list is derived from the binary's own `Subcommands:`
# block, so a build that feature-gates out `lsp` / `repl` is checked for
# exactly the surface it ships (specs/cli.md § `kio completions` documents
# that the advertised set follows the built-in features).
#
# Help-text *wording* is implementation-specific (the same convention
# specs/exit-codes.md applies to error messages), so this case pins the
# four §C2 *structural* elements per subcommand, not prose:
#   1. a `Usage: kio <name> …` synopsis as the first line;
#   2. an `Exit codes (per <BASE>/specs/exit-codes.md …):` block;
#   3. a `See <BASE>/specs/cli.md#<anchor> …` spec footer;
#   4. no bare relative `specs/…` path — every doc reference is the full
#      <BASE> GitHub URL.
# The top-level `kio --help` is §C4, not §C2: it carries the `See <BASE>`
# footer (no `#anchor`) and lists `--no-cache` under Options, but has no
# per-subcommand Exit-codes block.
#
# Two further conformance layers beyond the four structural elements:
#   - Anchor resolution: every `specs/cli.md#<anchor>` a footer emits must
#     name a real heading of specs/cli.md. The heading slugs are computed
#     here (lowercase; drop backticks / brackets / angle-brackets / dots;
#     spaces become dashes), so a footer pointing at a renamed or deleted
#     heading is caught as a dangling link.
#   - Container children: `kio doc`, `kio cache`, `kio dep`, and `kio sig`
#     each dispatch to child subcommands; their `kio <container> <child>
#     --help` surfaces get the same §C2 + anchor checks, so a child's
#     footer (e.g. `#kio-dep-fetch---force-name`) is conformance-checked
#     too, not just the container's.
#
# <BASE> (the pinned KIO_DOCS_BASE_URL) is read from the top-level footer
# rather than hardcoded, so this case tracks the base across a version bump
# instead of becoming an untracked version mirror — version-check.sh already
# pins the literal value against the repo version.
set -u

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

# --- Locate specs/cli.md (walk up from the case's working directory) ------
# This run.sh executes in place inside the repo checkout, so the repo's
# specs/cli.md sits at some ancestor's `specs/cli.md`.
cli=
walk=$(pwd)
while [ "$walk" != / ]; do
  if [ -f "$walk/specs/cli.md" ]; then
    cli="$walk/specs/cli.md"
    break
  fi
  walk=$(dirname "$walk")
done
[ -n "$cli" ] || fail "cannot locate specs/cli.md from $(pwd) (repo root not found)"

# --- Valid heading slugs of specs/cli.md ----------------------------------
# GitHub renders a `##`/`###` heading to an anchor slug: lowercase the
# text, drop backticks, brackets, angle-brackets, and dots, and turn each
# space into a dash. Every `See <BASE>/specs/cli.md#<anchor>` footer must
# name one of these.
valid_slugs=$(grep -E '^(##|###) ' "$cli" \
  | sed 's/^#* //' \
  | tr '[:upper:]' '[:lower:]' \
  | tr -d '`[]<>.' \
  | tr ' ' '-')
[ -n "$valid_slugs" ] || fail "no headings parsed from $cli"

# check_anchors <help-text> <label>: every `specs/cli.md#<anchor>` the help
# text emits must name a real heading slug of specs/cli.md.
check_anchors() {
  ca_text=$1
  ca_label=$2
  ca_anchors=$(printf '%s\n' "$ca_text" \
    | grep -oE '/specs/cli\.md#[A-Za-z0-9._-]+' \
    | sed 's|.*cli\.md#||')
  for ca_anchor in $ca_anchors; do
    printf '%s\n' "$valid_slugs" | grep -qxF "$ca_anchor" \
      || fail "$ca_label: 'specs/cli.md#$ca_anchor' names no heading in specs/cli.md (dangling anchor)"
  done
}

# check_surface <words>: the §C2 structural checks plus anchor resolution
# for one help surface. <words> is the subcommand path after `kio`, e.g.
# `check` or `doc build`.
check_surface() {
  cs_words=$1
  cs_head=${cs_words%% *}
  # shellcheck disable=SC2086 # cs_words is a controlled subcommand path.
  cs_out=$("$KIO_BIN" $cs_words --help) || fail "kio $cs_words --help: non-zero exit"
  [ -n "$cs_out" ] || fail "kio $cs_words --help: empty stdout"

  # §C2.1 — first line is a `Usage: kio …` synopsis. A container child that
  # routes to its parent's umbrella help (e.g. `kio sig stage`) opens with
  # the container synopsis `Usage: kio <container> …`; a subcommand with its
  # own synopsis opens with `Usage: kio <words> …`. Accept either.
  cs_first=$(printf '%s\n' "$cs_out" | sed -n '1p')
  case "$cs_first" in
    "Usage: kio $cs_words"*) ;;
    "Usage: kio $cs_head"*) ;;
    *) fail "kio $cs_words --help: first line is not a 'Usage: kio $cs_head …' synopsis" ;;
  esac

  # §C2.2 — Exit-codes block, URL-anchored to <BASE>.
  printf '%s\n' "$cs_out" | grep -qF "Exit codes (per $base/specs/exit-codes.md" \
    || fail "kio $cs_words --help: missing 'Exit codes (per <BASE>/specs/exit-codes.md …)' block"

  # §C2.3 — spec footer `See <BASE>/specs/cli.md#<anchor> …`.
  printf '%s\n' "$cs_out" | grep -qF "See $base/specs/cli.md#" \
    || fail "kio $cs_words --help: missing 'See <BASE>/specs/cli.md#<anchor>' footer"

  # §C2.4 — no bare `specs/…`: strip the full <BASE>/specs/ URLs, then any
  # residual `specs/` is a bare relative reference.
  if printf '%s\n' "$cs_out" | sed "s#$base/specs/##g" | grep -q 'specs/'; then
    fail "kio $cs_words --help: bare 'specs/…' reference (must be the full <BASE> URL)"
  fi

  # Anchor resolution — every cli.md anchor the footer emits is a real slug.
  check_anchors "$cs_out" "kio $cs_words --help"
}

# container_children <container>: the child subcommand names a container
# dispatches to. Prefer the `Subcommands:` block (`kio doc` / `cache` /
# `dep`); fall back to the multi-line `Usage:` synopsis (`kio sig`, which
# lists one `kio sig <child> …` line per child and no Subcommands block).
container_children() {
  cc_container=$1
  cc_help=$("$KIO_BIN" "$cc_container" --help 2>/dev/null) || return 1
  cc_kids=$(printf '%s\n' "$cc_help" \
    | awk '/^Subcommands:/{f=1;next} /^Options:|^Exit codes|^See /{f=0} f && /^  [a-z]/{print $1}')
  if [ -z "$cc_kids" ]; then
    cc_kids=$(printf '%s\n' "$cc_help" \
      | sed -n "s/^[[:space:]]*\(Usage:[[:space:]]*\)\{0,1\}kio $cc_container \([a-z][a-z-]*\).*/\2/p")
  fi
  printf '%s\n' "$cc_kids"
}

# --- Top-level `kio --help` (§C4) -----------------------------------------
top=$("$KIO_BIN" --help) || fail "kio --help: non-zero exit"
[ -n "$top" ] || fail "kio --help: empty stdout"

# Anchor <BASE> from the top-level `See <BASE>/specs/cli.md` footer.
base=$(printf '%s\n' "$top" | sed -n 's#^See \(https://[^ ]*\)/specs/cli\.md.*#\1#p')
case "$base" in
  https://*) ;;
  *) fail "kio --help: missing 'See <BASE>/specs/cli.md' footer (cannot anchor docs base)" ;;
esac

# §C4: the `--no-cache` global appears under Options.
printf '%s\n' "$top" | grep -q -- '--no-cache' \
  || fail "kio --help: '--no-cache' missing from Options"

# The top-level footer carries no `#anchor` today; validate defensively so a
# future anchored top-level footer is held to the same resolution contract.
check_anchors "$top" "kio --help"

# --- Advertised subcommand list (from the `Subcommands:` block) -----------
subs=$(printf '%s\n' "$top" | awk '/^Subcommands:/{f=1;next} /^Options:/{f=0} f && /^  [a-z]/{print $1}')
[ -n "$subs" ] || fail "kio --help: no subcommands parsed from the Subcommands: block"

# A help-parse regression would silently empty the loop, so require the
# always-present core subcommands (lsp / repl are feature-gated and only
# checked when the binary ships them).
for want in init check build fmt test sig doc cache dep completions; do
  printf '%s\n' "$subs" | grep -qxF "$want" \
    || fail "Subcommands: block missing core subcommand '$want' (help-parse regression?)"
done

# --- Per-subcommand `kio <sub> --help` (§C2 + anchor resolution) ----------
for sub in $subs; do
  check_surface "$sub"
done

# --- Container children `kio <container> <child> --help` (§C2 + anchors) --
# `doc` / `cache` / `dep` / `sig` are containers (asserted present in the
# core list above); a help-parse regression that emptied a container's
# child list would silently skip all its child coverage, so require each
# to yield children.
for container in doc cache dep sig; do
  kids=$(container_children "$container") \
    || fail "kio $container --help: non-zero exit"
  [ -n "$kids" ] || fail "kio $container --help: no child subcommands parsed (help-parse regression?)"
  for kid in $kids; do
    check_surface "$container $kid"
  done
done
