//! Prime-phase modules. The typer, lowering, and walking machinery is shared
//! by fresh-artifact validation, the full surface compiler, and the
//! `kio-prime` binary; the pipeline module is the Kio-prime-only frontend.
//!
//! - **`prime::lower`** — Surface → Prime walk shared by fresh Kio' artifact
//!   validation and the Kio-prime frontend. It rejects every
//!   surface-only variant with `Error::Parse` and emits Prime directly.
//!   The rejected set includes forms that the full pipeline removes through
//!   operator folding, desugaring, label elaboration, resolution, type-directed
//!   completion, and Lowered → Prime substitution; the Kio'-only route bypasses
//!   all of those transformations.
//! - **`prime::typer`** — the standalone Kio'-only typer
//!   implementation. Drives the shared scoped package checker over a
//!   `Package<Prime>`, with the recursive `synth_expr` /
//!   `check_value_against` calls landing in `PrimeTyper`'s
//!   `Typer<Prime>` impl. The full compiler uses it to validate
//!   substituted Prime; the Kio-prime compiler uses it after
//!   `prime::lower`. The package-level entry point is
//!   [`prime::typer::check_package`].
//! - **`prime::canonical`** — capture-avoiding statement-spine
//!   normalization shared by checked Prime module outputs and the
//!   Kio' emitter.
//! - **`prime::pipeline`** — `impl Pipeline for PrimePipeline`
//!   wiring the above into the [`crate::pipeline::Pipeline`]
//!   abstraction. The kio-prime binary's `check.rs` /
//!   `build.rs` / `fmt.rs` paths all dispatch through it.

pub(crate) mod canonical;
pub mod lower;
#[cfg(feature = "prime")]
pub mod pipeline;
pub mod typer;
pub mod walk;
