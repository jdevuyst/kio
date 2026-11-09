#!/bin/sh
# Nominal directives use their written compact declaration in the rendered site.
set -eu
trap 'rm -rf out/docs-md' EXIT

"$KIO_BIN" doc build --md
grep -Fq 'id="item-pkg-Foo"' out/docs-md/pkg.md
grep -Fq 'id="item-pkg-Bar"' out/docs-md/pkg.md
grep -Fq 'type { Foo: . }' out/docs-md/pkg.md
grep -Fq 'pub type Row = { Bar: . };' out/docs-md/pkg.md
if grep -Fq '@signature Foo' out/docs-md/pkg.md; then exit 1; fi
if grep -Fq '@source Bar' out/docs-md/pkg.md; then exit 1; fi
printf '%s\n' 'ordinary nominal references and directives rendered'
