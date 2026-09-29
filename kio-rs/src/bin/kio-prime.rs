#![cfg_attr(not(test), forbid(dead_code))]

//! `kio-prime` binary entry point — Kio'-only compiler.
//!
//! Shares the driver shell with `kio`; the only difference is
//! `prime_only = true`, which selects
//! [`kio_lang::prime::pipeline::PrimePipeline`] (Surface → Prime via
//! `prime::lower`, then the standalone `prime::typer`) instead of
//! [`kio_lang::full::FullPipeline`] (Surface → Lowered via
//! `desugar` + `label_elab`, then the elaboration-aware
//! `typecheck_full`). Surface-only forms reach the parse-error
//! path through `prime::lower`. See [`kio_lang::run`].

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    process::exit(kio_lang::run_on_worker(&args, true).as_i32());
}
