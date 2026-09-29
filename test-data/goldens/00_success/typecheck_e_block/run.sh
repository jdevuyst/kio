#!/bin/sh
# Subject: § 7.2 Blocks (specs/formal/elaboration.md).
#
# A block `{ stmt₁; …; stmtₖ; e }` desugars to a chain of E-Let
# rules — each `s;` becomes `let _ = s; rest` after `s`'s type
# is checked equal to `()`. The block's type is the synthesized
# type of the final expression.
#
# Cases:
#   - Empty stmt list: block is just the final expression.
#   - Single `let`: one E-Let layer, then the final expression.
#   - Expression statement `s;`: synthesized at `()` and discarded.
#   - Mixed `let` + expression statements.
#
# Each fn's body is a block whose synthesized type matches its
# declared return type; the discriminating shape is the
# `let`-then-expression-statement-then-final chain.
set -u
cd workdir || exit
"$KIO_BIN" check
