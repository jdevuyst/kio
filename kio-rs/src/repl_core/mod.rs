//! Shared REPL inspector core.
//!
//! This module contains the command parser/dispatcher, session model,
//! expression-query and purity-check paths, and plain/string highlighter used
//! by both the terminal `kio repl` front end and the browser wasm wrapper.
//! Terminal concerns such as reedline, history, prompts, and file watching stay
//! in [`crate::repl`].

pub mod commands;
pub mod completion;
pub mod expr_query;
pub mod highlight;
pub mod session;
