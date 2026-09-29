//! `kio-prime-check` CLI — verify that one or more `*.kio` files
//! parse against the formal Kio' grammar (see `specs/prime.md`
//! § Grammar).
//!
//! Used by `ci/prime-check.sh` to enforce, for every test case
//! carrying an `IS_KIO_PRIME` marker, that the case's regular-module
//! sources are syntactically Kio'.
//!
//! Usage: `kio-prime-check <FILE> [FILE...]`
//!
//! Exit codes: `0` if every file parsed; `1` if any failed; `2` for
//! usage errors.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use kio_prime_check::{ParseError, parse_module};

const USAGE: &str = "\
Usage: kio-prime-check <FILE> [FILE...]

Parse each FILE as a Kio' regular-module file (see specs/prime.md
§ Grammar). Reports per-file pass/fail to stderr; exits 0 if every
file parsed, 1 if any failed, 2 on a usage error.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        if args.is_empty() {
            eprintln!("error: no input files");
            eprintln!();
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        print!("{USAGE}");
        return ExitCode::from(0);
    }

    let mut had_failure = false;
    for arg in args {
        let path = PathBuf::from(&arg);
        match fs::read_to_string(&path) {
            Ok(src) => match parse_module(&src) {
                Ok(()) => {}
                Err(ParseError { offset, message }) => {
                    let (line, col) = line_col(&src, offset);
                    eprintln!("{}:{}:{}: {}", path.display(), line, col, message);
                    had_failure = true;
                }
            },
            Err(e) => {
                eprintln!("{}: {}", path.display(), e);
                had_failure = true;
            }
        }
    }

    if had_failure {
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}

/// Convert a byte offset to a 1-indexed (line, column) pair. Counts
/// columns in bytes, which is fine for the ASCII-leaning Kio' surface
/// — error messages just need a roughly-correct anchor.
fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(src.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, b) in src.as_bytes().iter().enumerate() {
        if i >= clamped {
            break;
        }
        if *b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
