//! Swift backend — profile and lowering entry points.
//!
//! Consumes a `Package<Routed>` (post-recovery — see
//! [`crate::pass::structural_recovery`]) and emits a self-contained Swift
//! package directory per the contract in
//! [`specs/backends/swift.md`](../../../../specs/backends/swift.md): a
//! multi-file Swift module (`pkg.swift`, `host.swift`, `shapes.swift`,
//! `ffi.swift`, `kio_runtime.swift`) plus complete `KioRuntime` support
//! sources. The package module name is the package's
//! namespace (default derivation and the `namespace` key:
//! [`crate::backends::namespace`]) — imposed at compile time via
//! `-module-name` and published in the `pkg.swift` marker line, since
//! Swift sources carry no module declaration — and which exposes a branded
//! `create<Handle>(host:)` factory, `<Handle>` being the namespace's
//! PascalCase.
//!
//! Swift's execution interfaces are **erased**: canonical native values flow
//! inside `Any`. Private witnesses retain the types selected at each lexical
//! type stage and construct exact product, sum, function, and nominal nodes.
//! Generic substitution therefore preserves the representation even beneath
//! opaque host applications; there is no payload-driven type reconstruction.
//!
//! Swift's skin is the **native-sum** end of the erased-static family:
//! sums become a native `enum` with associated values, matched by an
//! exhaustive `switch`; products become `struct`s; and the host record is a
//! `protocol`. Memory is ARC — automatic from the body's view.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub mod runtime;
pub mod skin;
mod value;

pub use emit::{EmitError, SwiftPackage, lower_package, lower_package_with_signature};
pub use runtime::{
    RUNTIME_SUPPORT_FILE_CONTENT, RUNTIME_SUPPORT_FILE_PATH, SHARED_RUNTIME_FILE_CONTENT,
    SHARED_RUNTIME_FILE_PATH,
};

/// The Swift backend's profile.
///
/// Swift has native records (`struct`) **and** native sums-with-payload
/// (`enum` with associated values, matched by an exhaustive `switch`) —
/// the native-sum end of the erased-static family. Field access is named
/// (`s.f0`). Swift requires explicit type declarations (the emitter
/// generates binary generic structural carriers and declaration-owned nominal
/// carriers). Swift manages memory with ARC — automatic from the
/// body's perspective, so the body threads no ownership annotations
/// (`gc: true`).
///
/// `runtime_support: EmbeddedFile` supplies package-local nominal storage;
/// the output also includes the shared callable/control module.
pub fn profile() -> Profile {
    Profile {
        native_records: true,
        native_sums: true,
        native_match: true,
        field_access: FieldAccessStyle::Named,
        requires_explicit_type_decls: true,
        gc: true,
        runtime_support: RuntimeSupport::EmbeddedFile {
            path: runtime::RUNTIME_SUPPORT_FILE_PATH,
        },
    }
}
