//! Compilation pipeline phases.
//!
//! These modules implement the following source-to-backend order (some
//! branches and post-Prime passes are conditional on the selected binary or
//! backend):
//!
//! 1. [`lexer`] — UTF-8 bytes → token stream.
//! 2. [`tree_skeleton`] — token stream → CST (lossless concrete-
//!    syntax tree with balanced groups; recovers locally on
//!    unbalanced brackets).
//! 3. [`parser`] — CST → `Module<Surface>` AST.
//! 4. Full Kio runs [`op_fold`], then [`desugar`] (`Surface →
//!    Desugared`) and [`label_elab`] (`Desugared → Lowered`). The
//!    Kio'-only front-end instead uses `prime::lower` to reject
//!    surface-only forms while converting `Surface → Prime`.
//! 5. [`resolve`] builds the package's ordinary name and import scopes;
//!    `alpha_normalize` gives lexical type binders stable identities.
//! 6. [`typecheck_core`] / [`typecheck_full`] perform bidirectional
//!    checking. The full path records every completion that must be
//!    materialized at the phase boundary.
//! 7. [`substitute`] consumes those records in a `Lowered → Prime`
//!    rewrite. The standalone Prime checker validates the assembled artifact,
//!    bakes its narrow call/lambda completion records, and
//!    `prime::canonical` normalizes the checked statement spine. The Kio'-only
//!    path starts at this standalone-check step.
//! 8. [`structural_recovery`] converts `Prime → Enriched`, and [`optimize`]
//!    performs backend-neutral optimizations over that enriched IR.
//! 9. [`recover_to_low`] converts `Enriched → Routed` and classifies call
//!    sites into `Expr::Low*` variants for host-backend dispatch.
//! 10. [`capabilities`] annotates the Routed package before host emission.
//!     The Kio' backend instead emits validated, canonical Prime directly.
//!
//! [`full`] hosts the `FullPipeline` implementation for the kio binary. The
//! kio-prime binary uses
//! `prime::pipeline::PrimePipeline` instead (see [`crate::prime`]).

pub(crate) mod alpha_normalize;
pub(crate) mod binder_presentation;
pub mod capabilities;
pub mod lexer;
pub mod optimize;
pub mod parser;
pub(crate) mod placeholder;
#[cfg(any(feature = "surface", feature = "cli"))]
pub(crate) mod rec_headers;
pub mod recover_to_low;
pub mod resolve;
pub mod structural_recovery;
pub mod tree_skeleton;
pub mod typecheck_core;
#[doc(hidden)]
pub mod visit_mut;

#[cfg(feature = "surface")]
pub(crate) mod block_projection;
#[cfg(feature = "surface")]
pub mod desugar;
#[cfg(feature = "surface")]
pub mod elaborator_registry;
#[cfg(feature = "surface")]
pub mod full;
#[cfg(feature = "surface")]
pub mod label_elab;
#[cfg(feature = "surface")]
pub mod op_fold;
#[cfg(feature = "surface")]
pub mod substitute;
#[cfg(any(feature = "surface", feature = "cli"))]
pub mod surface_registry;
#[cfg(feature = "surface")]
pub mod typecheck_full;
