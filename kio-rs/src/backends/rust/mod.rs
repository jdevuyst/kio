//! Rust backend — profile and lowering entry points.
//!
//! Consumes a `Package<Routed>` (post-recovery and post-resolution-lowering —
//! see [`crate::pass::structural_recovery`] and
//! [`crate::pass::recover_to_low`]) and emits a self-contained Cargo crate per
//! the contract in
//! [`specs/backends/rust.md`](../../../../specs/backends/rust.md):
//! `Cargo.toml`, `src/lib.rs`, `src/shapes.rs`, `src/host.rs`.
//!
//! A shape the emitter cannot render returns a structured
//! "unsupported" [`emit::EmitError`] rather than emitting wrong
//! code. The Rust spec page is the contract this module is built
//! against.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub mod runtime;
pub mod thread_safety;

pub use runtime::{
    RUNTIME_SUPPORT_FILE_CONTENT, RUNTIME_SUPPORT_FILE_PATH, RUNTIME_SUPPORT_MODULE,
};
pub use thread_safety::ThreadSafety;

/// The Rust backend's profile.
///
/// Rust has native records (`struct`), native sums-with-payload
/// (`enum`), and native pattern matching (`match`). Field access
/// is named for structs / tuple-positional (`.0`) for tuples;
/// the profile picks `Named` as the dominant style — exported items
/// surface as Rust structs / enums per
/// [`specs/backends/rust.md`](../../../../specs/backends/rust.md)
/// § Structural and nominal types. Rust requires explicit type
/// declarations (the lowering generates one Rust item per kio
/// structural shape and per `newtype`). Rust is not
/// garbage-collected; the emit threads ownership / lifetimes
/// where required.
pub fn profile() -> Profile {
    Profile {
        native_records: true,
        native_sums: true,
        native_match: true,
        field_access: FieldAccessStyle::Named,
        requires_explicit_type_decls: true,
        gc: false,
        // Rust's strict typing forces a non-trivial `Rc<dyn Fn>` /
        // `Rc<dyn Any>` wrap at every fn-value / existential
        // slot. The wrappers reduce to named calls into
        // `src/__kio_runtime.rs` — see `runtime` submodule. The
        // emitter writes that file in every emitted crate; the
        // build-side dispatcher reads
        // [`RUNTIME_SUPPORT_FILE_PATH`] and
        // [`RUNTIME_SUPPORT_FILE_CONTENT`] from `runtime` to land
        // it.
        runtime_support: RuntimeSupport::EmbeddedFile {
            path: runtime::RUNTIME_SUPPORT_FILE_PATH,
        },
    }
}

pub use emit::{EmitError, LowerOptions, RustCrate, lower_package, lower_package_with_options};
