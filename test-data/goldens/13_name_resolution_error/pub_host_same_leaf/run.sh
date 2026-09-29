#!/bin/sh
# A bridged module declaring two top-level items with the same leaf
# name `foo` (here `host fn foo` + `pub fn foo`) is rejected at name
# resolution (exit 13) — the per-module top-level scope keys on the leaf
# across every item kind. This shadows the package-export
# duplicate-name contract check (`resolve.rs`'s
# `validate_bridge_contract`), which is why that check asserts the case
# unreachable: every same-(module, leaf) `pub` collision is caught here
# first. This golden guards that ordering — if the top-level scope ever
# split by namespace, this case would change tier and the contract
# check's `unreachable!` would start firing.
set -u
cd workdir || exit
"$KIO_BIN" check
