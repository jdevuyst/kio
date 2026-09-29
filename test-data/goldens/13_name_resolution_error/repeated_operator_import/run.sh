#!/bin/sh
set -eu

cd workdir
"${KIO_BIN:-kio}" check
