#!/bin/sh
#
# Check that the repo-wide version number is consistent across all
# its mirror points.
#
# Mirror points (today):
#   - kio-rs/Cargo.toml
#   - kio-rs/dist/Cargo.toml
#   - kio-repl-wasm/Cargo.toml
#   - website/package.json
#   - ci/infra/kio-gen-rs/Cargo.toml
#   - ci/infra/kio-ci-scheduler-rs/Cargo.toml
#   - ci/infra/kio-prime-check-rs/Cargo.toml
#   - ci/infra/kio-test-runner-rs/Cargo.toml
#   - tools/tree-sitter-kio/package.json
#   - tools/tree-sitter-kio/tree-sitter.json (metadata.version field)
#   - tools/vscode-kio/package.json
#   - tools/textmate-kio/kio.tmLanguage.json (top-level "version" field)
#   - ci/infra/highlight-agreement-js/package.json
#   - README.md ("Current version: ..." line near the top)
#   - kio-rs/src/lib.rs (KIO_DOCS_BASE_URL — the
#     `blob/releases/v<major>.<minor>.<patch>` tag the `--help` strings
#     link the specs against; pinned to the current release tag).
#   - the tracked lockfiles: kio-rs/Cargo.lock, kio-rs/dist/Cargo.lock,
#     kio-repl-wasm/Cargo.lock, ci/infra/{kio-gen-rs,
#     kio-ci-scheduler-rs,kio-prime-check-rs,kio-test-runner-rs}/Cargo.lock
#     (each lock's own crate entry), and
#     website/, tools/vscode-kio/, ci/infra/highlight-agreement-js/
#     package-lock.json (root "version").
#
# Tracked lockfiles also embed the repo version (each Cargo.lock's own
# crate entry; each package-lock.json's root "version") and are
# asserted as mirrors below: a version bump that skips refreshing a
# committed lockfile would otherwise ship a stale embedded version
# that nothing checks until the next build touches it. A bump PR
# refreshes them via `cargo update -p <crate>` (or any build) and
# `npm install --package-lock-only`.
#
# Adding a new mirror = appending a (file, extractor) record below.
# `extractor` is a sed script that prints exactly the version string
# from one line in the file.
#
# Pinned-version manifest (NOT a mirror of the repo version):
#   - kio-rs/fuzz/Cargo.toml — the cargo-fuzz harness is `publish = false`
#     and pinned at "0.0.0" by cargo-fuzz convention, so it is exempt from
#     the repo-version agreement check above and instead gets a dedicated
#     equals-"0.0.0" assertion at the end of this script (still gated so an
#     accidental bump is caught).
#
# Non-publishable fixture descriptors (NOT mirrors of the repo version):
#   - test-data/emissions/*/*/host/Cargo.toml
#   - test-data/emissions/*/*/host/package.json
#   - test-data/emissions/*/*/host/go.mod
# These describe host programs compiled only by their emissions fixtures;
# they are not independently released subprojects.
#
# Independent pinned-version set — the `dist` release-tool version (its
# own version, unrelated to the repo version, so it is NOT in the mirror
# list above). It is pinned in three places that must agree:
#   - dist-workspace.toml            (`cargo-dist-version`)
#   - .github/workflows/release.yml  (installer download URL tag)
#   - mise.optional.toml             (`aqua:axodotdev/cargo-dist`)
# `dist generate` rewrites the first two from the config in lockstep, but
# mise.optional.toml is hand-maintained, so nothing tied it to the other
# two. The fourth pass below asserts the three agree with each other (no
# hard-coded expected: an intentional dist bump just has to update all
# three, which is exactly the discipline the check enforces).
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

# (file, extractor-sed-script) pairs, one per line, separated by a tab.
TAB=$(printf '\t')
mirrors="\
kio-rs/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
kio-rs/dist/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
kio-repl-wasm/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
website/package.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
ci/infra/kio-gen-rs/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
ci/infra/kio-ci-scheduler-rs/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
ci/infra/kio-prime-check-rs/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
ci/infra/kio-test-runner-rs/Cargo.toml${TAB}/^version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
tools/tree-sitter-kio/package.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
tools/tree-sitter-kio/tree-sitter.json${TAB}/^    \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
tools/vscode-kio/package.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
tools/textmate-kio/kio.tmLanguage.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
ci/infra/highlight-agreement-js/package.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
README.md${TAB}/Current version:/{s/.*Current version:[ *]*\\([0-9][0-9]*[.][0-9][0-9]*[.][0-9][0-9]*\\).*/\\1/p;q;}
kio-rs/src/lib.rs${TAB}/KIO_DOCS_BASE_URL/{s|.*/blob/releases/v\\([0-9][0-9]*\\.[0-9][0-9]*\\.[0-9][0-9]*\\)\".*|\\1|p;q;}
kio-rs/Cargo.lock${TAB}/^name = \"kio-lang\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
kio-rs/dist/Cargo.lock${TAB}/^name = \"kio\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
kio-repl-wasm/Cargo.lock${TAB}/^name = \"kio-repl-wasm\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
ci/infra/kio-gen-rs/Cargo.lock${TAB}/^name = \"kio-gen\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
ci/infra/kio-ci-scheduler-rs/Cargo.lock${TAB}/^name = \"kio-ci-scheduler\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
ci/infra/kio-prime-check-rs/Cargo.lock${TAB}/^name = \"kio-prime-check\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
ci/infra/kio-test-runner-rs/Cargo.lock${TAB}/^name = \"kio-test-runner-rs\"\$/{n;s/^version = \"\\(.*\\)\"\$/\\1/p;q;}
website/package-lock.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
tools/vscode-kio/package-lock.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}
ci/infra/highlight-agreement-js/package-lock.json${TAB}/^  \"version\":/{s/.*\"version\" *: *\"\\([^\"]*\\)\".*/\\1/p;q;}"

# First pass — print each mirror's version for visibility, and bail
# loudly if any mirror is missing or the extractor finds nothing.
printf '%s\n' "$mirrors" | while IFS="$TAB" read -r file extractor; do
  [ -n "$file" ] || continue
  if [ ! -f "$file" ]; then
    printf 'version-check: %s: file not found\n' "$file" >&2
    exit 1
  fi
  version=$(sed -n "$extractor" "$file")
  if [ -z "$version" ]; then
    printf 'version-check: %s: extractor matched no line (sed=%s)\n' "$file" "$extractor" >&2
    exit 1
  fi
  printf '  %-40s %s\n' "$file" "$version"
done

# Second pass — compare each mirror to the first version we saw.
# Runs inside a `{ … }` group so the loop's accumulator survives the
# pipe. Reports each disagreement with both files named, and exits
# non-zero if any mismatch was seen.
expected=""
expected_file=""
fail=0
printf '%s\n' "$mirrors" | {
  while IFS="$TAB" read -r file extractor; do
    [ -n "$file" ] || continue
    version=$(sed -n "$extractor" "$file")
    if [ -z "$expected" ]; then
      expected=$version
      expected_file=$file
    elif [ "$version" != "$expected" ]; then
      printf '\nversion-check: mismatch — %s says %s but %s says %s\n' \
        "$expected_file" "$expected" "$file" "$version" >&2
      fail=1
    fi
  done
  exit "$fail"
}

# Third pass — the cargo-fuzz harness is exempt from the repo-version
# agreement above (publish=false, pinned at 0.0.0 by cargo-fuzz
# convention), so assert it equals its own dedicated expected value.
# This still gates an accidental bump while keeping it out of the mirror
# list (which would otherwise force it to track the repo version).
fuzz_manifest="kio-rs/fuzz/Cargo.toml"
fuzz_expected="0.0.0"
if [ ! -f "$fuzz_manifest" ]; then
  printf 'version-check: %s: file not found\n' "$fuzz_manifest" >&2
  exit 1
fi
fuzz_version=$(sed -n '/^version *= *"/{s/.*"\(.*\)".*/\1/p;q;}' "$fuzz_manifest")
if [ -z "$fuzz_version" ]; then
  printf 'version-check: %s: extractor matched no version line\n' "$fuzz_manifest" >&2
  exit 1
fi
printf '  %-40s %s (pinned)\n' "$fuzz_manifest" "$fuzz_version"
if [ "$fuzz_version" != "$fuzz_expected" ]; then
  printf '\nversion-check: %s says %s but the cargo-fuzz harness must stay pinned at %s\n' \
    "$fuzz_manifest" "$fuzz_version" "$fuzz_expected" >&2
  exit 1
fi

# Fourth pass — the `dist` release-tool version, pinned in three files
# that must agree with each other (see the header note). This is its own
# agreement set, separate from the repo-version mirrors above, because
# `dist` versions independently of the repo.
dist_mirrors="\
dist-workspace.toml${TAB}/^cargo-dist-version *= *\"/{s/.*\"\\(.*\\)\".*/\\1/p;q;}
.github/workflows/release.yml${TAB}/cargo-dist\\/releases\\/download/{s|.*/download/v\\([0-9][^/]*\\)/.*|\\1|p;q;}
mise.optional.toml${TAB}/axodotdev\\/cargo-dist/{s/.*= *\"\\([^\"]*\\)\".*/\\1/p;q;}"

dist_expected=""
dist_expected_file=""
dist_fail=0
printf '%s\n' "$dist_mirrors" | {
  while IFS="$TAB" read -r file extractor; do
    [ -n "$file" ] || continue
    if [ ! -f "$file" ]; then
      printf 'version-check: %s: file not found\n' "$file" >&2
      exit 1
    fi
    version=$(sed -n "$extractor" "$file")
    if [ -z "$version" ]; then
      printf 'version-check: %s: dist-version extractor matched no line (sed=%s)\n' \
        "$file" "$extractor" >&2
      exit 1
    fi
    printf '  %-40s %s (dist)\n' "$file" "$version"
    if [ -z "$dist_expected" ]; then
      dist_expected=$version
      dist_expected_file=$file
    elif [ "$version" != "$dist_expected" ]; then
      printf '\nversion-check: dist-version mismatch — %s says %s but %s says %s\n' \
        "$dist_expected_file" "$dist_expected" "$file" "$version" >&2
      dist_fail=1
    fi
  done
  exit "$dist_fail"
}
