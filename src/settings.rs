//! What a program built on ax decides for it.
//!
//! On its own, ax keeps the person's accounts in their data folder, watches
//! the login in `~/.claude`, and tells them to run `ax …`. A program that
//! embeds it — one with accounts of its own, working as a login of its own,
//! reached under a name of its own — says so here, once, before it does
//! anything else. Nothing is read from the environment for this: variables
//! set in a process to reach a crate it links leak into everything that
//! process starts.

use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Default, Clone)]
pub struct Settings {
    /// Where the accounts and their profiles live, instead of the person's
    /// ax folder.
    pub data_dir: Option<PathBuf>,
    /// Which Claude Code config folder — which login — is the current one,
    /// instead of `CLAUDE_CONFIG_DIR` or `~/.claude`.
    pub claude_config_dir: Option<PathBuf>,
    /// What someone types to reach these commands, for messages that tell
    /// them what to run next: `anna claude`, say. Told to run `ax account
    /// add` instead, they would add the account to the wrong store.
    pub invoked_as: Option<String>,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Once, before anything reads a path. A second call is a programming error
/// and says so, rather than quietly leaving the first in place.
pub fn configure(settings: Settings) {
    if SETTINGS.set(settings).is_err() {
        panic!("ax was configured twice");
    }
}

pub fn settings() -> &'static Settings {
    static ON_ITS_OWN: Settings = Settings { data_dir: None, claude_config_dir: None, invoked_as: None };
    SETTINGS.get().unwrap_or(&ON_ITS_OWN)
}
