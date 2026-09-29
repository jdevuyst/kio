//! Python backend — profile and lowering entry points.
//!
//! The Python backend is type-erased: generated package code embeds the
//! routed Kio package as backend-neutral IR and evaluates it with a small
//! Python runtime. The host-facing boundary remains shaped by the shared
//! backend contract: products and sums cross as keyed dictionaries,
//! a newtype whose constructor and projector are both public crosses under
//! its FFI key, every other public newtype crosses as a declaration-specific
//! nominal carrier, function values are bridged at call time, and the package
//! exposes a branded `create_<stem>(host)` factory (the module stem is the
//! package namespace — the kio package name by default, or the `namespace`
//! build-block key).
//!
//! Alongside the runtime `<stem>.py`, the backend emits a `<stem>/` typed-stub
//! package ([`stub`]) — the Python analogue of the TypeScript `.d.ts`. Its
//! `__init__.pyi` and private shard modules contain declarations only (no
//! runtime code), typing the same public surface `create_<stem>` exposes so a
//! pyright / mypy host checks the embedding at author time. Type checkers
//! select the stub package while Python selects the sibling runtime module.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub mod stub;

pub use emit::{EmitError, lower_package_to_module, lower_package_to_module_with_signature};
pub use stub::{
    PythonStubPackage, lower_package_to_stub, lower_package_to_stub_package,
    lower_package_to_stub_package_with_signature, lower_package_to_stub_with_signature,
};

pub const PROFILE: Profile = Profile {
    native_records: false,
    native_sums: false,
    native_match: false,
    field_access: FieldAccessStyle::Bracket,
    requires_explicit_type_decls: false,
    gc: true,
    runtime_support: RuntimeSupport::None,
};
