#!/bin/sh
set -eu

# Calling a projected callback retains its ordinary-call boundary after formatting.
formatted=$("$KIO_BIN" fmt - <<'KIO'
mod api;
pub type { Vault[A]: A { mk; get } }
pub fn run(vault: Vault[. -> .]) { (vault.>Vault.get)() }
pub fn unit(vault: Vault[. -> .]) { (vault.>Vault.get)(()) }
pub fn value[A](vault: Vault[A -> A], value: A) -> A { (vault.>Vault.get)(value) }
KIO
)
printf '%s\n' "$formatted" | "$KIO_BIN" fmt -
