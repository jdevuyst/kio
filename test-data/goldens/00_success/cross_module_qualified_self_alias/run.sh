#!/bin/sh
# Regression: a cross-module transparent type alias whose body is the
# QUALIFIED, same-named type (`cmqsa/n` declares `pub type A = m.A`,
# where `m.A` is the root module's `A`; the alias `A` shadows the leaf
# name). A consumer that imports the alias (`import cmqsa/n(A)`) and
# uses it would send the typer into an infinite loop: alias unfolding
# looked the alias up by its bare leaf name, so the qualified body `m.A`
# collapsed back to leaf `A`, re-matched the same alias, and never
# terminated. The unfold loop now stops re-expanding an alias body it
# has already seen, so `A` resolves to the root's `A` and `kio check`
# terminates. Both `kio` and `kio-prime` share the affected unfold code,
# so IS_KIO_PRIME runs this under the standalone Kio' checker too.
set -u
cd workdir || exit
"$KIO_BIN" check
