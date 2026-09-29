//! JavaScript backend — profile and lowering entry points.
//!
//! Implementation lives in [`emit`], which consumes
//! `Module<Routed>` (post-resolution-lowering) and writes a branded
//! `create<Handle>(host)` ES-module factory (the handle is the
//! PascalCase of the artifact stem). Each `Expr::Low*` variant
//! produced by [`crate::pass::recover_to_low::lower`] has a per-variant
//! emit helper in `emit`; the renderer drops to syntactic
//! templating with no per-call-site classification. This wrapper
//! exposes the per-backend convention every backend follows: a
//! [`profile()`] descriptor plus the lowering's `lower_*` entry
//! points exported by name. The framework in [`super`] documents
//! the contract.
//!
//! The FFI shape — what a host sees at the boundary — is settled in
//! [`specs/backends/js.md`](../../../../specs/backends/js.md):
//! right-spine-flattened objects for products / sums, JS-native
//! types per the `role(...)` table, JS functions for exported fns. The
//! per-signature wrapper at every exported-fn boundary converts the
//! internal nested-binary array rep to that FFI shape.

pub mod emit;

use super::{FieldAccessStyle, Profile, RuntimeSupport};

/// The JS backend's profile.
///
/// `native_records = true` because JS objects are records, but the
/// emitter's *internal* rep is nested binary arrays — the FFI
/// wrapper converts to the spec's `{tag: value, …}` shape at the
/// boundary. `native_sums = false` because JS has no sum-with-
/// payload primitive — the internal rep uses `[tag, payload]` pairs
/// and the FFI wrapper surfaces the single-keyed-object spec shape.
/// `native_match = false` — JS has no pattern-match; tag dispatch
/// renders as a ternary chain.
pub fn profile() -> Profile {
    Profile {
        native_records: true,
        native_sums: false,
        native_match: false,
        field_access: FieldAccessStyle::Bracket,
        requires_explicit_type_decls: false,
        gc: true,
        // JS has native fn values and dynamic typing — every
        // wrap / erase / identity helper is a one-liner the
        // per-variant emit writes inline. No runtime-support file
        // is written. See `super` module's § Runtime-support
        // library convention.
        runtime_support: RuntimeSupport::None,
    }
}

pub use crate::backends::js::emit::{
    EmitError, lower_package_to_factory_module, lower_package_to_factory_module_cached,
};
