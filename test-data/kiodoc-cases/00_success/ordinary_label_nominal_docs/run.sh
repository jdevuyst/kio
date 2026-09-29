#!/bin/sh
# Nominal directives use their written labels declaration in the rendered site.
set -eu
trap 'rm -rf out/docs-md' EXIT

"$KIO_BIN" doc build --md
grep -Fq 'id="item-pkg-Foo"' out/docs-md/pkg.md
grep -Fq 'id="item-pkg-Bar"' out/docs-md/pkg.md
grep -Fq 'labels { foo: . };' out/docs-md/pkg.md
grep -Fq 'pub labels Row = { bar: . };' out/docs-md/pkg.md
if grep -Fq '@signature Foo' out/docs-md/pkg.md; then exit 1; fi
if grep -Fq '@source Bar' out/docs-md/pkg.md; then exit 1; fi
printf '%s\n' 'ordinary label nominal references and directives rendered'
