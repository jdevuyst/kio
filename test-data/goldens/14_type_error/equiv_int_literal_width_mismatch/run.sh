#!/bin/sh
# Ground literals of the same digits but different roles are distinct
# (specs/formal/equiv.md § 4.5: `42^i32 ≄ 42^i64`, same digits,
# different role). An `equiv` block's arms must share a static type, so
# arm 2 (`1(U16)`) checked against arm 1's type `U8` is a type error
# (exit 14): the width distinction surfaces at type-check, before
# discharge is ever reached. The reachable *discharge* failures (same
# role, different digit / value) are pinned by
# 50_test_failure/equiv_int_literal_distinct and
# equiv_bool_literal_distinct.
set -u
cd workdir || exit
"$KIO_BIN" test
