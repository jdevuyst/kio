//! kio-gen — typed generator + differential test harness for Kio.
//!
//! Emits well-typed Kio programs as case directories consumed by
//! `ci/run-tests.sh`. Independent of `kio-rs`: the generator
//! builds typed terms in its own AST and renders them to Kio source
//! text, which keeps it credible as a cross-implementation oracle.
//!
//! Productions cover Kio' (literals, lambdas, applications, `let`,
//! polymorphism, the six type-form intrinsics plus
//! `__if_then_else__`, explicitly recursive singleton newtypes and mutual type
//! groups, multi-module
//! packages) and the surface forms `if!` blocks, tuple literals,
//! `{label = e}` label-value sugar, `match!`, the algebraic / spine
//! elaborators, user-defined `elab` declarations/calls, and operators.
//! `Program::uses_surface` records surface forms in the generated function body;
//! `Program::surface_mode` controls their package-level supporting declarations.
//! `--prime-only` restricts emission to the Kio' core.

extern crate self as kio_gen;

pub mod ast;
pub mod emit;
pub mod generate;
pub mod mutate;
pub mod render;
pub mod shrink;

/// Deterministic per-program seed: combine the run seed with the
/// program index using SplitMix64. Parallelism does not affect
/// output because each program is generated from a seed derived
/// from `(run_seed, idx)` rather than from shared RNG state.
pub fn program_seed(run_seed: u64, idx: u64) -> u64 {
    let mut z = run_seed.wrapping_add(idx.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
