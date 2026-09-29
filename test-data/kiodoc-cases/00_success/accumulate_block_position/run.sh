#!/bin/sh
# A block-position harness wraps __INSERT_CODE_HERE__ inside a
# `pub fn main() -> .` body. Each member contributes block
# contents (let bindings, expression statements). Combined with
# accumulate, multiple members assemble one block in document
# order.
set -u
"$KIO_BIN" doc check
