#!/bin/sh
# Literal-role admission (specs/language.md § Literals): an integer
# literal admits integer- and float-shaped roles only, never the
# string role. Annotating `42` as `Str` (role(str)) is a type error
# (exit 14); the admission relation rejects it independent of context.
# The ok-twin, admission through a role(i32)-inheriting alias, is
# 00_success/exec_literal_rehost_alias_admits_int.
set -u
cd workdir || exit
"$KIO_BIN" check
