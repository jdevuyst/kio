#!/bin/sh
# Literal-role admission (specs/language.md § Literals): integer
# literals admit integer-shaped roles only. Annotating `42` as `Str`
# (role(str)) keeps the incompatible string-shaped role explicit.
# The ok-twin, admission through a role(i32)-inheriting alias, is
# 00_success/exec_literal_rehost_alias_admits_int.
set -u
cd workdir || exit
"$KIO_BIN" check
