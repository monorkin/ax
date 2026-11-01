//! ax as a library: stored Claude Code and Codex accounts, switching between them, and
//! watching their usage, for programs that build on it. The `ax` binary is a
//! thin command line over this crate.

pub mod account;
pub mod auto_switch;
pub mod claude;
pub mod cli;
pub mod clock;
pub mod codex;
pub mod completions;
pub mod fsutil;
pub mod locks;
pub mod login;
pub mod mappings;
pub mod oauth;
pub mod paths;
pub mod pool;
pub mod session;
pub mod settings;
pub mod store;
pub mod usage;
