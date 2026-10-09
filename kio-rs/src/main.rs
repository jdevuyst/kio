#![cfg_attr(not(test), forbid(dead_code))]

//! `kio` binary entry point — full-language Kio compiler.
//!
//! See [`kio_lang::run`] for the shared driver shell. The companion
//! `kio-prime` binary lives at `src/bin/kio-prime.rs` and differs
//! only in the `prime_only = true` argument it threads through —
//! that flag selects
//! [`kio_lang::prime::pipeline::PrimePipeline`] over
//! [`kio_lang::full::FullPipeline`] inside the driver.

use std::env;
use std::process;

mod manual;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let manual_args: Vec<_> = args
        .iter()
        .filter(|arg| arg.as_str() != "--no-cache")
        .cloned()
        .collect();
    if manual_args.first().is_some_and(|arg| arg == "manual") {
        process::exit(manual::run(&manual_args[1..]));
    }
    process::exit(kio_lang::run_on_worker(&args, false).as_i32());
}
