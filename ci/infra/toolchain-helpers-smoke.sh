#!/bin/sh
# General helpers retain their separate store and fail closed on partial restores.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
owner=$repo_root/ci/impl-toolchain.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
sh "$owner" toolchain-general-install js,rust -- tree-sitter 'github:mszostok/codeowners-validator[exe=codeowners-validator]'
sh "$owner" toolchain-exec js --helpers 'github:mszostok/codeowners-validator[exe=codeowners-validator]' -- \
  sh -c 'command -v codeowners-validator'
sh "$owner" toolchain-exec js --helpers tree-sitter -- npm --prefix "$repo_root/tools/vscode-kio" --version
helper=$(sh "$owner" toolchain-exec js --helpers tree-sitter -- sh -c 'command -v tree-sitter')
helper_install=${helper%/*}
mkdir -p "$work/general/installs/tree-sitter"
cp -R "$helper_install" "$work/general/installs/tree-sitter/"
private_helper="$work/general/installs/tree-sitter/${helper_install##*/}/${helper##*/}"
KIO_GENERAL_MISE_DATA_DIR="$work/general" sh "$owner" toolchain-exec js --helpers tree-sitter -- tree-sitter --version
mv "$private_helper" "$private_helper.saved"
if KIO_GENERAL_MISE_DATA_DIR="$work/general" sh "$owner" toolchain-exec js --helpers tree-sitter -- \
    sh -c ': >"$1"' sh "$work/subject-ran" >"$work/missing.log" 2>&1; then
  printf 'error: partial helper restore executed its subject\n' >&2; exit 1
fi
[ ! -e "$work/subject-ran" ]
ln -s "$helper" "$private_helper"
if KIO_GENERAL_MISE_DATA_DIR="$work/general" sh "$owner" toolchain-exec js --helpers tree-sitter -- \
    sh -c ': >"$1"' sh "$work/subject-ran" >"$work/origin.log" 2>&1; then
  printf 'error: escaped helper symlink executed its subject\n' >&2; exit 1
fi
[ ! -e "$work/subject-ran" ]
mkdir -p "$work/caller-cargo/bin"
printf '%s\n' '#!/bin/sh' 'echo "ambient cargo-mutants selected" >&2' 'exit 97' >"$work/caller-cargo/bin/cargo-mutants"
chmod +x "$work/caller-cargo/bin/cargo-mutants"
# shellcheck disable=SC2046 # The owner emits native tool IDs, without shell syntax.
sh "$owner" toolchain-general-install rust --environment optional -- $(sh "$owner" report-mise-tools)
CARGO_HOME="$work/caller-cargo" PATH="$work/caller-cargo/bin:$PATH" \
  sh "$owner" toolchain-exec rust --environment optional --reports -- cargo mutants --version
sh "$owner" toolchain-exec python --environment python3_10 --helpers pipx:mypy -- mypy --version
sh "$owner" toolchain-env python --environment python3_10 >"$work/python.sh"
PYTHONHOME="$work/missing-python" sh -c '. "$1"; [ -z "${PYTHONHOME:-}" ]; python3 -c "import json; print(42)"' sh "$work/python.sh"
GITHUB_ENV="$work/github-env" sh "$owner" toolchain-github-env python --environment python3_10
grep -qx 'PYTHONHOME=' "$work/github-env"
printf 'toolchain helpers smoke: minimal npm/tree-sitter, incomplete/escaped restore rejection and exact optional report precedence passed\n'
