#!/bin/sh
set -eu

cd workdir
exec "${KIO_BIN:-kio}" check
