//! Java backend — profile and lowering entry points.
//!
//! The Java backend emits a four-file typed facade over a serialized-IR
//! interpreter body — the `erased-static` family's declared divergence:
//! the body strategy is an interpreter rather than per-function compiled
//! code, while the observable contract stays the family's
//! (the **Family divergence — body strategy** paragraph in
//! `specs/backends/java.md`,
//! `specs/backends/README.md` § Language families). It emits: the branded package handle (`<Handle>.java`), the
//! host contract (`<Handle>Host.java`), the boundary shape declarations
//! (`Shapes.java`), and the interpreter (`KioRuntime.java`), all under
//! the package namespace's directory. The body evaluates embedded
//! backend-neutral IR at runtime; the skin converts typed Java values to
//! the interpreter's documented boundary shapes.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub mod skin;

pub use emit::{EmitError, JavaPackage, lower_package};

pub fn profile() -> Profile {
    Profile {
        native_records: false,
        native_sums: false,
        native_match: false,
        field_access: FieldAccessStyle::Bracket,
        requires_explicit_type_decls: true,
        gc: true,
        // The interpreter ships as one of the backend's own four output
        // files (`KioRuntime.java`, under the package-namespace
        // directory), written by the java build arm — not through the
        // framework's fixed-path `EmbeddedFile` convention.
        runtime_support: RuntimeSupport::None,
    }
}
