//! ax as a library: stored Claude Code accounts, switching between them, and
//! watching their usage, for programs that build on it. The `ax` binary is a
//! thin command line over this crate.

pub mod account;
pub mod auto_switch;
pub mod claude;
pub mod cli;
pub mod clock;
pub mod completions;
pub mod fsutil;
pub mod locks;
pub mod mappings;
pub mod oauth;
pub mod paths;
pub mod session;
pub mod store;
