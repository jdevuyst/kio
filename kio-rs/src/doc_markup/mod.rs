//! Shared source-neutral documentation syntax and inert presentation.

pub(crate) mod blocks;
pub(crate) mod editor;
pub(crate) mod fences;
pub(crate) mod generated;
pub(crate) mod lexical;
pub(crate) mod rewrite;

pub(crate) mod source;

/// Native presentation falls back to literal text for deeper authored nesting.
pub(crate) const MAX_RENDER_DEPTH: usize = 64;
