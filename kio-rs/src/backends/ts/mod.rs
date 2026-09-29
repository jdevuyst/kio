//! TypeScript backend — profile and lowering entry points.
//!
//! TypeScript is a **pure-skin** target: it adds no body emitter at
//! all. The runtime artifact is the JS backend's `<ns>.js`,
//! byte-identical (`kio build ts` reuses
//! [`crate::backends::js::emit::lower_package_to_factory_module`] with
//! the same resolved namespace). The only new artifact is a generated
//! `<ns>.d.ts` sidecar — TypeScript's type-annotations-for-JS mechanism
//! — which declares the typed FFI skin: the `<Handle>Host<B>` type,
//! the `create<Handle>` factory, the exported package surface (`<Handle>`),
//! and the structural shapes that cross the boundary (every name branded
//! from the artifact stem). The `.d.ts` renderer ([`emit::lower_package_to_dts`])
//! consumes the package-complete [`crate::backends::boundary_facade::PreparedBoundaryCallableSites`]
//! catalog. Sealed signature history may add optional deprecated host
//! properties and their frozen type dependencies to that declaration skin;
//! the JS runtime remains current-only and unchanged.
//!
//! Because the body is the JS backend's verbatim, the skin's value
//! representation is identical to JS's: products are `{ _0, _1, … }`
//! objects keyed by the right-spine slot, sums are single-keyed
//! objects, newtypes are `{ <ffi_key>: payload }`, atomics are
//! JS-native primitives. The `.d.ts` renders the TypeScript *types* of
//! exactly those shapes (`specs/backends/ts.md`) from the prepared semantic
//! plans and exact nominal inventory, without reconstructing public topology
//! from JS rendering spellings.
//!
//! The FFI shape — what a host sees at the boundary — is settled in
//! [`specs/backends/ts.md`](../../../../specs/backends/ts.md). The
//! framework in [`super`] documents the per-backend convention every
//! backend follows.

pub mod emit;

use super::{FieldAccessStyle, Profile, RuntimeSupport};

/// The TypeScript backend's profile.
///
/// Mirrors the JS profile ([`crate::backends::js::profile`]) because the
/// runtime body *is* the JS backend's `.js`, byte-identical: the
/// `typed-dynamic` family erases that runtime, which has native object records
/// and bracket access, no sum-with-payload primitive, no pattern-match, and is
/// garbage-collected, while the declaration skin remains exact.
/// `runtime_support: None` — the `.js` it ships needs no support file, and the
/// `.d.ts` adds no runtime helpers.
pub fn profile() -> Profile {
    Profile {
        native_records: true,
        native_sums: false,
        native_match: false,
        field_access: FieldAccessStyle::Bracket,
        requires_explicit_type_decls: false,
        gc: true,
        // The runtime artifact is the JS backend's `.js`; the `.d.ts`
        // skin adds no runtime helpers. No runtime-support file.
        runtime_support: RuntimeSupport::None,
    }
}

pub use crate::backends::ts::emit::{
    EmitError, lower_package_to_dts, lower_package_to_dts_with_signature,
};
