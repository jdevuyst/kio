#!/bin/sh
# One requested name exists in only one package; the other exists nowhere.
cd workdir || exit 1
exec "$KIO_BIN" build --pkg app web --pkg other absent
