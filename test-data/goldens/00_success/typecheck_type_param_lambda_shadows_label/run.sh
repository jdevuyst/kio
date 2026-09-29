#!/bin/sh
# Subject: a lambda type-parameter shadows a label-generated type with the
# same spelling. The label `a` introduces generated type `A`; the lambda's
# `[A]` binder must scope over its parameter and return annotations.
set -u
cd workdir || exit
"$KIO_BIN" check
