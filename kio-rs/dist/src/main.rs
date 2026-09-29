//! `kio` command entry point for the distributable build.
//!
//! A thin wrapper over [`kio_lang::run_on_worker`], identical to
//! `kio-rs`'s own `kio` entry point. This crate exists only so `dist`
//! packages the `kio` binary alone; the `kio-prime` tool lives in
//! `kio-rs` and stays out of the release. See `kio-rs/src/main.rs`.

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    process::exit(kio_lang::run_on_worker(&args, false).as_i32());
}
