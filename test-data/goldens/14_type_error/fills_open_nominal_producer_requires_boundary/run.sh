#!/bin/sh
# A pending callback keeps an open nominal producer ineligible for the action.
set -u
cd workdir || exit
"$KIO_BIN" check
