#!/bin/sh
# shellcheck disable=SC2016 # Markdown backticks are literal assertion text.
set -eu
trap 'rm -rf out' EXIT

"$KIO_BIN" doc check
"$KIO_BIN" doc build --html --md

grep -F 'An operator cannot contain `[` or `]`.' out/docs-md/input.md >/dev/null
grep -F 'Quoted `` [`missing`] `` and `` [`@signature missing`] `` are examples.' out/docs-md/input.md >/dev/null
grep -F '[`identity`](pkg.md#item-pkg-identity)' out/docs-md/input.md >/dev/null
grep -F '## [`identity`](pkg.md#item-pkg-identity)' out/docs-md/input.md >/dev/null
grep -F '`pub fn identity(x: .) -> .`' out/docs-md/input.md >/dev/null
grep -F 'Brackets `[` and `]` are literal code.' out/docs-md/pkg.md >/dev/null
grep -F 'Quoted `` [`missing`] `` and `` [`@signature missing`] `` are examples.' out/docs-md/pkg.md >/dev/null
grep -F '<code>[</code> or <code>]</code>' out/docs/input.html >/dev/null

printf 'inline code and real links preserved\n'
