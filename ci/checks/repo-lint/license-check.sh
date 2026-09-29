#!/bin/sh
#
# Verify the repo's dual-license metadata mirrors agree.
#
# The repo is dual-licensed MIT OR Apache-2.0 (README.md § License;
# LICENSE-APACHE + LICENSE-MIT at the root), and every subproject
# manifest mirrors that as one uniform SPDX expression. Canonical
# mirror list:
#   - kio-rs/Cargo.toml
#   - kio-rs/dist/Cargo.toml
#   - kio-repl-wasm/Cargo.toml
#   - ci/infra/kio-gen-rs/Cargo.toml
#   - ci/infra/kio-ci-scheduler-rs/Cargo.toml
#   - ci/infra/kio-prime-check-rs/Cargo.toml
#   - ci/infra/kio-test-runner-rs/Cargo.toml
#   - website/package.json
#   - tools/tree-sitter-kio/package.json
#   - tools/tree-sitter-kio/tree-sitter.json (metadata license field)
#   - tools/vscode-kio/package.json
#   - ci/infra/highlight-agreement-js/package.json
#
# Documented exclusions:
#   - kio-rs/fuzz/Cargo.toml — `publish = false` cargo-fuzz scaffolding,
#     excluded from the version mirror list for the same reason.
#   - test-data/emissions/*/*/host/Cargo.toml and package.json —
#     non-publishable host-fixture descriptors, not subproject manifests or
#     repo metadata mirrors.
#   - tools/textmate-kio/kio.tmLanguage.json — a grammar data file, not
#     a manifest: it carries the mirrored "version" field but the
#     tmLanguage format has no license slot, so the repo LICENSE files
#     govern it.
#
# The root deliberately ships LICENSE-APACHE + LICENSE-MIT and no
# combined LICENSE / COPYING / COPYRIGHT file: license detectors rank
# those filenames first and content-match them against canonical
# texts, so a prose dual-license notice under one of those names
# degrades detection from "Apache-2.0 + MIT" to "unknown". README.md
# § License carries the human-readable notice.
# kio-rs mirrors both texts because Cargo packages only files within its
# crate root.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

SPDX='MIT OR Apache-2.0'
failures=0

for f in LICENSE-APACHE LICENSE-MIT; do
  if [ ! -f "$f" ]; then
    printf 'license-check: missing license text: %s\n' "$f" >&2
    failures=1
  fi
  if [ ! -f "kio-rs/$f" ]; then
    printf 'license-check: missing crate license text: kio-rs/%s\n' "$f" >&2
    failures=1
  elif [ -f "$f" ] && ! cmp -s "$f" "kio-rs/$f"; then
    printf 'license-check: crate license text differs from root: kio-rs/%s\n' "$f" >&2
    failures=1
  fi
done

for f in LICENSE LICENSE.md LICENSE.txt COPYING COPYRIGHT; do
  if [ -e "$f" ]; then
    printf 'license-check: %s exists at the root — the repo deliberately has no combined license file (see this script header)\n' "$f" >&2
    failures=1
  fi
done

cargo_mirrors='kio-rs/Cargo.toml
kio-rs/dist/Cargo.toml
kio-repl-wasm/Cargo.toml
ci/infra/kio-gen-rs/Cargo.toml
ci/infra/kio-ci-scheduler-rs/Cargo.toml
ci/infra/kio-prime-check-rs/Cargo.toml
ci/infra/kio-test-runner-rs/Cargo.toml'

json_mirrors='website/package.json
tools/tree-sitter-kio/package.json
tools/tree-sitter-kio/tree-sitter.json
tools/vscode-kio/package.json
ci/infra/highlight-agreement-js/package.json'

printf '%s\n' "$cargo_mirrors" | while read -r f; do
  [ -n "$f" ] || continue
  if [ ! -f "$f" ]; then
    printf 'license-check: mirror not found: %s\n' "$f" >&2
    exit 1
  fi
  if ! grep -qx "license = \"$SPDX\"" "$f"; then
    printf 'license-check: %s: missing or wrong license field (want: license = "%s")\n' "$f" "$SPDX" >&2
    exit 1
  fi
done

printf '%s\n' "$json_mirrors" | while read -r f; do
  [ -n "$f" ] || continue
  if [ ! -f "$f" ]; then
    printf 'license-check: mirror not found: %s\n' "$f" >&2
    exit 1
  fi
  if ! grep -qF "\"license\": \"$SPDX\"" "$f"; then
    printf 'license-check: %s: missing or wrong license field (want: "license": "%s")\n' "$f" "$SPDX" >&2
    exit 1
  fi
done

# A tracked manifest outside the mirror list means a new subproject
# landed without joining the license (and likely version) mirrors.
git ls-files 'Cargo.toml' '*/Cargo.toml' 'package.json' '*/package.json' | while read -r f; do
  case "$f" in
    kio-rs/fuzz/Cargo.toml) ;;
    test-data/emissions/*/*/host/Cargo.toml|test-data/emissions/*/*/host/package.json) ;;
    kio-rs/Cargo.toml|kio-rs/dist/Cargo.toml|kio-repl-wasm/Cargo.toml) ;;
    ci/infra/kio-gen-rs/Cargo.toml|ci/infra/kio-ci-scheduler-rs/Cargo.toml|ci/infra/kio-prime-check-rs/Cargo.toml|ci/infra/kio-test-runner-rs/Cargo.toml) ;;
    website/package.json|tools/tree-sitter-kio/package.json|tools/vscode-kio/package.json|ci/infra/highlight-agreement-js/package.json) ;;
    *)
      printf 'license-check: tracked manifest not in the mirror list: %s — add it here and to version-check.sh\n' "$f" >&2
      exit 1
      ;;
  esac
done

if [ "$failures" -ne 0 ]; then
  exit 1
fi
