#!/bin/sh
# A [`@type term`] directive in .md prose whose term resolves to a
# `type` is a Kiodoc contract error (exit 70): a type binds
# no value. `@type` expects a value binding.
set -u
"$KIO_BIN" doc check
