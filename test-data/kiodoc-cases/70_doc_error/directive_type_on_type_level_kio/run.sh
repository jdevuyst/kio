#!/bin/sh
# A [`@type term`] directive whose term resolves to a type-level name
# (type / labels / newtype) is a Kiodoc contract error (exit 70): a
# type-level name binds no value. `@type` expects a value binding.
set -u
"$KIO_BIN" doc check
