#!/bin/sh
# Small native witnesses for installer acceptance; no Kio build or corpus run.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)

if [ "${1:-}" = --witness ]; then
  languages=$2
  work=$3
  cd "$work"
  for language in $(printf '%s' "$languages" | tr ',' ' '); do
    case "$language" in
      js) node -e 'if (20 + 22 !== 42) process.exit(1); console.log(42)' ;;
      ts)
        printf '%s\n' 'const answer: number = 20 + 22; if (answer !== 42) throw Error("answer"); console.log(answer);' >witness.ts
        tsc --strict --target es2020 --module commonjs witness.ts
        node witness.js
        mise which node
        mise which tsc ;;
      python)
        case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) python=python ;; *) python=python3 ;; esac
        "$python" -c 'answer = 20 + 22; assert answer == 42; print(answer)'
        case "$(command -v pyright)" in "$KIO_TOOLCHAINS_ROOT/mise/data/installs/npm-pyright/"*/bin/pyright*) ;; *) exit 1 ;; esac
        printf '%s\n' 'answer: int = 20 + 22' >witness.py
        pyright --pythonpath "$(command -v "$python")" witness.py
        mise which pyright ;;
      rust)
        printf '%s\n' 'fn main() { assert_eq!(20 + 22, 42); println!("42"); }' >witness.rs
        rustc witness.rs -o rust-witness
        ./rust-witness
        rustc --print sysroot
        mise exec rust -- rustc --print sysroot ;;
      go)
        printf '%s\n' 'package main' 'import "fmt"' 'func main() { if 20 + 22 != 42 { panic("answer") }; fmt.Println(42) }' >witness.go
        go build -o go-witness witness.go
        ./go-witness ;;
      java)
        printf '%s\n' 'class Witness { public static void main(String[] args) { if (20 + 22 != 42) throw new AssertionError(); System.out.println(42); } }' >Witness.java
        javac Witness.java
        java Witness ;;
      swift)
        printf '%s\n' 'import Foundation' 'precondition(20 + 22 == 42)' 'print(42)' >witness.swift
        swiftc witness.swift -o swift-witness
        ./swift-witness ;;
      haskell)
        printf '%s\n' 'module Main where' 'main :: IO ()' 'main = if 20 + 22 == (42 :: Integer) then print (42 :: Integer) else fail "answer"' >Witness.hs
        GHC_PACKAGE_PATH="$(ghc --print-global-package-db)" ghc -no-user-package-db -package-env - -v0 Witness.hs -o haskell-witness
        ./haskell-witness ;;
      *) printf 'error: unknown witness language: %s\n' "$language" >&2; exit 2 ;;
    esac
  done
  exit 0
fi

languages=${1:?provide explicit comma-separated languages}
environment=${2:-development}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
printf 'installer smoke: OS=%s architecture=%s environment=%s\n' "$(uname -s)" "$(uname -m)" "$environment"
sh "$repo_root/ci/impl-toolchain.sh" toolchain-install "$languages" --environment "$environment"
sh "$repo_root/ci/impl-toolchain.sh" toolchain-exec "$languages" --environment "$environment" -- \
  sh "$0" --witness "$languages" "$work"
# The second setup must reuse the same exact installations and remain executable.
sh "$repo_root/ci/impl-toolchain.sh" toolchain-install "$languages" --environment "$environment"
sh "$repo_root/ci/impl-toolchain.sh" toolchain-exec "$languages" --environment "$environment" -- \
  sh "$0" --witness "$languages" "$work"
printf 'installer smoke: pass\n'
