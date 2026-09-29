//! Go backend — profile and lowering entry points.
//!
//! Consumes a `Package<Routed>` (post-recovery — see
//! [`crate::pass::structural_recovery`]) and emits a self-contained Go
//! package directory per the contract in
//! [`specs/backends/go.md`](../../../../specs/backends/go.md): a
//! multi-file Go package (`pkg.go`, `host.go`, `shapes.go`, `ffi.go`,
//! `kio_runtime.go`) whose `package` clause is the package's namespace
//! (default derivation and the `namespace` key:
//! [`crate::backends::namespace`]) and which exposes a branded
//! `Create<Handle>(host)` factory, `<Handle>` being the namespace's
//! PascalCase.
//!
//! Go's body is **erased** — every value flows as `interface{}` /
//! `[]any`, the JS dynamic body's shape on a statically-typed host.
//! `interface{}` is a genuine dynamic universal carrier: scalar
//! polymorphism (rank-N / existential) and higher-kinded carriers alike
//! erase to it, so `Maybe(String)` and the dictionary-internal
//! `Maybe(any)` share one uniform `[]any` rep and a higher-kinded
//! re-key needs **no carrier-walk** (the JS dynamic body's story, in
//! Go). Go, Rust, and Swift all erase their bodies to a dynamic
//! universal carrier this way; a carrier-walk is load-bearing only for
//! a **native-HKT** host (Haskell) that threads a typed carrier through
//! its own type-constructor application — the erased body is
//! host-invisible, so erasure costs nothing the host sees
//! (`ai/topics/emit.md` § Runtime model). The hard part of this backend
//! is the **typed FFI skin**, not the body.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub(crate) mod facade;
pub(crate) mod facade_skin;
pub(crate) mod naming;
pub mod runtime;

pub use emit::{EmitError, GoPackage, lower_package, lower_package_with_signature};
pub use runtime::{RUNTIME_SUPPORT_FILE_PATH, runtime_support_content};

/// The Go backend's profile.
///
/// Go has native records (`struct`) but **no** sum-with-payload
/// primitive. The typed FFI realizes products as generic structs and a
/// concrete sum as `KioSum[Row]`; its private case storage is inspected
/// through `Case()` and a Go type switch. Field access is named (`s.F0`).
/// Go requires explicit type declarations, which the prepared boundary
/// facade realizes from semantic shell, row, and nominal identities. Go is
/// garbage-collected, so the body threads no ownership annotations.
///
/// `runtime_support: EmbeddedFile` — Go has no built-in unit value, and the
/// erased body uses fixed product/sum carrier adapters. The canonical `Unit`
/// plus those structural helpers are declared once per emitted package in
/// `kio_runtime.go`, rather than repeated across its generated files.
pub fn profile() -> Profile {
    Profile {
        native_records: true,
        native_sums: false,
        native_match: false,
        field_access: FieldAccessStyle::Named,
        requires_explicit_type_decls: true,
        gc: true,
        runtime_support: RuntimeSupport::EmbeddedFile {
            path: runtime::RUNTIME_SUPPORT_FILE_PATH,
        },
    }
}
