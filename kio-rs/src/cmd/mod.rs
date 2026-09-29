//! Subcommand handlers for the `kio` and `kio-prime` binaries.
//!
//! Each child module here is the implementation of one `kio
//! <subcommand>`. The top-level [`crate::run`] dispatcher matches
//! the argv head and calls into the matching module's `run`.

pub(crate) mod atomic_write;
pub mod build;
pub(crate) mod build_timing;
pub mod cache;
pub mod check;
pub mod completions;
pub mod debug;
pub mod dep;
pub mod fmt;
pub mod init;
pub mod module_selector;
pub mod package_fanout;

#[cfg(feature = "surface")]
pub mod sig;

#[cfg(feature = "surface")]
pub mod test;
