//! Haskell backend — profile and lowering entry points.
//!
//! Consumes a `Package<Routed>` (post-recovery — see
//! [`crate::pass::structural_recovery`]) and emits a self-contained
//! Haskell package directory: one self-contained package module `<Ns>.hs`
//! containing the host record, the `<Handle>` handle, the `create<Handle>`
//! factory, every module fn, and unexported private runtime support. The
//! namespace `<Ns>` defaults to the package name PascalCased and is overridden
//! by the `namespace` build key ([`crate::backends::namespace`]); the branded
//! `<Handle>` is its final segment.
//!
//! Haskell is the **native-HKT** family. Unlike Swift / Go — which erase
//! the body to a dynamic universal carrier (`Any` / `any`) — Haskell
//! renders the Routed IR at its **native** types: anonymous products and
//! sums use closed type families over type-level lists with boundary-local
//! aliases and patterns, matches use native pattern matching, and a
//! higher-kinded carrier `F(A)` → native `f a` (no brand-marker, no
//! carrier-walk, no erasure — a typed carrier rides Haskell's own
//! type-constructor application). Where an *erased-static* host (Go /
//! Rust / Swift) carries `F(A)` as the universal erased value and
//! downcasts at the apply, Haskell threads the carrier in its own types,
//! so neither erasure nor a re-key is needed.
//!
//! Every emitted function is **monad-polymorphic** (`Monad m => …`); the
//! host record is a **value** record of `m`-returning functions (not a
//! typeclass); the exported surface is `Monad m => …`. Kio is **strict**:
//! host effects sequence through the monad (`>>=` / `do`) for effect
//! order, and bound values are forced (strict binds / a private helper) for
//! value strictness in Haskell's lazy host.

use super::{FieldAccessStyle, Profile, RuntimeSupport};

pub mod emit;
pub(crate) mod facade;
pub(crate) mod naming;
pub mod native;
pub mod reconstruct;
pub mod runtime;
pub mod skin;

pub use emit::{EmitError, HaskellPackage, lower_package, lower_package_with_signature};
pub(crate) use runtime::runtime_declarations;

/// The Haskell backend's profile.
///
/// Haskell has native products **and** native sums-with-payload, so the
/// native-HKT family renders the IR at native types rather than erasing
/// it. Boundary-local pattern synonyms provide named field access while
/// closed structural families keep substitution stable. Haskell requires
/// explicit type declarations for these shared families, their boundary
/// aliases and patterns, and Kio `newtype` declarations.
/// Haskell is garbage-collected (`gc: true`).
///
/// `runtime_support: None` — the backend writes no separate support file.
/// Its namespace-derived private value-strictness helper and universal carrier
/// are ordinary unexported declarations in the self-contained `<Ns>.hs`
/// facade. The spec page's § Output layout is the reader-facing artifact
/// contract.
pub fn profile() -> Profile {
    Profile {
        native_records: true,
        native_sums: true,
        native_match: true,
        field_access: FieldAccessStyle::Named,
        requires_explicit_type_decls: true,
        gc: true,
        runtime_support: RuntimeSupport::None,
    }
}
