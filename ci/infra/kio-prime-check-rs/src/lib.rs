//! Syntactic verifier for Kio'.
//!
//! Implements the grammar specified in `specs/prime.md` § Grammar.
//! Used by `ci/prime-check.sh` to enforce, for every test case
//! carrying an `IS_KIO_PRIME` marker, that the case's regular-module
//! `*.kio` sources parse against the formal Kio' grammar.
//!
//! Independent of `kio-rs` — this crate's parser is the *second*
//! Kio' parser in the tree. The redundancy is the point: it lets the
//! corpus check act as an oracle on `kio-rs` rather than asking
//! `kio-rs` to test itself.

#![forbid(unsafe_code)]

mod lexer;
mod parser;

pub use parser::{ParseError, parse_module};
