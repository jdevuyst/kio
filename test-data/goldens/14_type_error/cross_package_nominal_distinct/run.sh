#!/bin/sh
# Cross-package nominal distinctness: the consumer and its
# `textdep` dependency each declare a `String` host type. Identity-exact
# nominal identity keys on (module, name), so the two are distinct and
# feeding the consumer's `String` to the dependency's `dep_string`
# (which wants the dependency's `String`) is a type error — exit 14. A
# bare-last-segment typer wrongly conflates the two by leaf name.
set -u
cd workdir || exit
"$KIO_BIN" check
