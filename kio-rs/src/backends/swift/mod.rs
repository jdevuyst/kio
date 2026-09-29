//! Swift backend — profile and lowering entry points.
//!
//! Consumes a `Package<Routed>` (post-recovery — see
//! [`crate::pass::structural_recovery`]) and emits a self-contained Swift
//! package directory per the contract in
//! [`specs/backends/swift.md`](../../../../specs/backends/swift.md): a
//! multi-file Swift module (`pkg.swift`, `host.swift`, `shapes.swift`,
//! `ffi.swift`, `kio_runtime.swift`) whose module name is the package's
//! namespace (default derivation and the `namespace` key:
//! [`crate::backends::namespace`]) — imposed at compile time via
//! `-module-name` and published in the `pkg.swift` marker line, since
//! Swift sources carry no module declaration — and which exposes a branded
//! `create<Handle>(host:)` factory, `<Handle>` being the namespace's
//! PascalCase.
//!
//! Swift's body is **erased** — every value flows as `Any` / `[Any]`,
//! the JS dynamic body's shape on a statically-typed host. `Any` is a
//! genuine dynamic universal carrier: scalar polymorphism (rank-N /
//! existential) and higher-kinded carriers alike erase to it, so
//! `Maybe(String)` and the dictionary-internal `Maybe(Any)` share one
//! uniform `[Any]` rep and a higher-kinded re-key needs **no
//! carrier-walk** (the JS dynamic body's story, in Swift). Go, Rust, and
//! Swift all erase their bodies to a dynamic universal carrier this way;
//! a carrier-walk is load-bearing only for a **native-HKT** host
//! (Haskell) that threads a typed carrier — the erased body is
//! host-invisible, so erasure costs nothing the host sees
//! (`ai/topics/emit.md` § Runtime model). The hard part of this target is
//! the **typed FFI skin**, not the body.
//!
//! Swift's skin is the **native-sum** end of the erased-static family:
//! sums become a native `enum` with associated values, matched by an
//! exhaustive `switch`; products become `struct`s; and the host record is a
//! `protocol`. Memory is ARC — automatic from the body's view.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub mod runtime;
pub mod skin;

pub use emit::{EmitError, SwiftPackage, lower_package, lower_package_with_signature};
pub use runtime::{RUNTIME_SUPPORT_FILE_CONTENT, RUNTIME_SUPPORT_FILE_PATH};

/// The Swift backend's profile.
///
/// Swift has native records (`struct`) **and** native sums-with-payload
/// (`enum` with associated values, matched by an exhaustive `switch`) —
/// the native-sum end of the erased-static family. Field access is named
/// (`s.f0`). Swift requires explicit type declarations (the emitter
/// generates one Swift type per Kio structural shape the FFI surfaces and
/// per `newtype`). Swift manages memory with ARC — automatic from the
/// body's perspective, so the body threads no ownership annotations
/// (`gc: true`).
///
/// `runtime_support: EmbeddedFile` — the erased body needs no carrier
/// helpers, but the canonical `Unit` and the erased value plumbing are
/// worth naming once in `kio_runtime.swift` rather than re-declaring per
/// package, mirroring Rust's `src/__kio_runtime.rs`.
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
